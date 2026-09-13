//! Tests for the packager.
//!
//! Every bundle layout is built here, for all three platforms, whatever the host is — that is
//! the point of keeping the container step separate.
//!
//! **Every case sets `seal: false`,** so nothing here runs `appimagetool` or WiX. That is not
//! squeamishness about subprocesses: a test that seals when the tool happens to be installed
//! and stages when it does not is a test of the runner. CI found that the hard way — the
//! GitHub Windows image ships WiX, so the one case that reached the sealing path passed on
//! this laptop, where WiX is absent, and failed there.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A directory that removes itself, so a failing test does not leave litter that the next
/// run then merges with.
struct Temp(PathBuf);

impl Temp {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "crisol-package-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A plausible set of options, with a real file standing in for the executable.
fn options(temp: &Temp, target: Platform) -> Options {
    let binary = temp.path().join("todo");
    fs::write(&binary, b"\x7fELF not really").expect("a stand-in binary");
    Options {
        binary,
        name: "Todo".to_owned(),
        identifier: "com.example.todo".to_owned(),
        version: "1.2.3".to_owned(),
        out: temp.path().join("dist"),
        target,
        icon: None,
        manufacturer: None,
        upgrade_code: None,
        seal: false,
    }
}

// ---- validation ---------------------------------------------------------------------

#[test]
fn a_missing_binary_is_refused_before_anything_is_written() {
    let temp = Temp::new("nobin");
    let mut opts = options(&temp, Platform::MacOs);
    opts.binary = temp.path().join("does-not-exist");
    assert!(matches!(build(&opts), Err(Error::NoBinary(_))));
    assert!(
        !opts.out.exists(),
        "nothing should be created for options that cannot describe a bundle"
    );
}

#[test]
fn a_name_with_a_separator_is_not_a_bundle_name() {
    let temp = Temp::new("badname");
    for bad in ["", "a/b", "a\\b"] {
        let mut opts = options(&temp, Platform::MacOs);
        opts.name = bad.to_owned();
        assert!(
            matches!(build(&opts), Err(Error::BadName(_))),
            "{bad:?} should be refused"
        );
    }
}

#[test]
fn an_identifier_has_to_be_reverse_dns() {
    assert!(is_reverse_dns("com.example.todo"));
    assert!(is_reverse_dns("com.example"));
    assert!(is_reverse_dns("com.example.todo-app"));
    assert!(!is_reverse_dns("todo"), "one segment is not reverse-DNS");
    assert!(!is_reverse_dns("com..todo"), "an empty segment");
    assert!(!is_reverse_dns("com.example."), "a trailing dot");
    assert!(!is_reverse_dns("com.-example"), "a leading hyphen");
    assert!(!is_reverse_dns("com.exa mple"), "a space");
}

#[test]
fn a_version_is_dot_separated_numbers() {
    assert!(check_dotted_version("1").is_ok());
    assert!(check_dotted_version("1.2.3.4").is_ok());
    assert!(check_dotted_version("").is_err());
    assert!(check_dotted_version("1.").is_err());
    assert!(check_dotted_version("1.x").is_err());
    assert!(check_dotted_version("v1.2").is_err());
}

// ---- the MSI version ceiling, which is the one that fails silently ------------------

#[test]
fn an_msi_version_field_may_not_exceed_what_the_installer_can_hold() {
    assert!(
        windows::check_version("255.255.65535").is_ok(),
        "the ceiling"
    );
    assert!(windows::check_version("1.2.3").is_ok());

    assert!(windows::check_version("256.0.0").is_err(), "major");
    assert!(windows::check_version("1.256.0").is_err(), "minor");
    assert!(windows::check_version("1.2.65536").is_err(), "build");
    assert!(
        windows::check_version("1.2.3.4").is_err(),
        "a fourth field is ignored when comparing, so two releases would collide"
    );
}

#[test]
fn windows_will_not_invent_an_upgrade_code() {
    let temp = Temp::new("noupgrade");
    let opts = options(&temp, Platform::Windows);
    assert!(opts.upgrade_code.is_none());
    assert!(matches!(build(&opts), Err(Error::MissingUpgradeCode)));
}

// ---- escaping, which is where a user-chosen name gets to break a parser -------------

#[test]
fn a_name_that_is_not_xml_safe_is_escaped_into_the_plist() {
    let temp = Temp::new("escape");
    let mut opts = options(&temp, Platform::MacOs);
    opts.name = "Tom & Jerry <\"quoted\">".to_owned();
    let plist = macos::info_plist(&opts, "todo", None);
    assert!(plist.contains("Tom &amp; Jerry &lt;&quot;quoted&quot;&gt;"));
    assert!(
        !plist.contains("Tom & Jerry"),
        "the raw ampersand would make the plist unparseable"
    );
}

#[test]
fn a_desktop_entry_escapes_with_backslashes_not_entities() {
    let temp = Temp::new("desktop-escape");
    let mut opts = options(&temp, Platform::Linux);
    opts.name = "Line\nBreak & Co".to_owned();
    let entry = linux::desktop_entry(&opts, "todo");
    assert!(entry.contains("Name=Line\\nBreak & Co"));
    assert!(
        !entry.contains("&amp;"),
        "a desktop entry is not XML, and escaping it as though it were corrupts the name"
    );
    assert_eq!(
        entry
            .lines()
            .filter(|line| line.starts_with("Name="))
            .count(),
        1,
        "an unescaped newline would split the value into a second, bogus key"
    );
}

// ---- the layouts --------------------------------------------------------------------

#[test]
fn a_macos_bundle_has_the_shape_macos_requires() {
    let temp = Temp::new("app");
    let opts = options(&temp, Platform::MacOs);
    let produced = build(&opts).expect("a bundle");

    assert_eq!(produced.artifact, opts.out.join("Todo.app"));
    assert!(
        produced.missing_tool.is_none(),
        "nothing external is needed"
    );

    let contents = produced.artifact.join("Contents");
    assert!(contents.join("Info.plist").is_file());
    assert!(contents.join("MacOS").join("todo").is_file());
    assert!(contents.join("Resources").is_dir());
    assert_eq!(
        fs::read_to_string(contents.join("PkgInfo")).expect("PkgInfo"),
        "APPL????"
    );

    // The plist has to name the file that is actually there, or macOS refuses to launch it.
    let plist = fs::read_to_string(contents.join("Info.plist")).expect("the plist");
    assert!(plist.contains("<key>CFBundleExecutable</key>\n    <string>todo</string>"));
    assert!(plist.contains("<string>com.example.todo</string>"));
    assert!(plist.contains("<string>1.2.3</string>"));
    assert!(
        !plist.contains("CFBundleIconFile"),
        "no icon was given, so the key must be absent rather than empty"
    );
}

#[test]
fn an_icon_is_named_by_the_plist_only_when_there_is_one() {
    let temp = Temp::new("icon");
    let icon = temp.path().join("source.icns");
    fs::write(&icon, b"icns").expect("an icon");
    let mut opts = options(&temp, Platform::MacOs);
    opts.icon = Some(icon);

    let produced = build(&opts).expect("a bundle");
    let contents = produced.artifact.join("Contents");
    assert!(contents.join("Resources").join("Todo.icns").is_file());
    let plist = fs::read_to_string(contents.join("Info.plist")).expect("the plist");
    assert!(plist.contains("<key>CFBundleIconFile</key>\n    <string>Todo.icns</string>"));
}

#[test]
fn an_appdir_carries_its_launcher_and_both_desktop_entries() {
    let temp = Temp::new("appdir");
    let opts = options(&temp, Platform::Linux);
    let produced = build(&opts).expect("an AppDir");

    let dir = opts.out.join("Todo.AppDir");
    assert_eq!(produced.artifact, dir);
    assert!(produced.missing_tool.is_none());

    assert!(dir.join("usr").join("bin").join("todo").is_file());
    assert!(dir.join("com.example.todo.desktop").is_file());
    assert!(
        dir.join("usr")
            .join("share")
            .join("applications")
            .join("com.example.todo.desktop")
            .is_file(),
        "the installed copy, which is what a menu reads"
    );

    let run = fs::read_to_string(dir.join("AppRun")).expect("AppRun");
    assert!(run.starts_with("#!/bin/sh"));
    assert!(
        run.contains("readlink -f"),
        "AppImage reaches the launcher through a symlink; $0's directory is the mount, not the payload"
    );
    assert!(run.contains("exec \""), "the app must replace the shell");
    assert!(
        run.contains("\"$@\""),
        "arguments must survive, spaces and all"
    );
}

#[test]
fn a_wxs_names_the_staged_file_relatively() {
    let temp = Temp::new("wxs");
    let mut opts = options(&temp, Platform::Windows);
    opts.upgrade_code = Some("12345678-1234-1234-1234-123456789012".to_owned());
    let produced = build(&opts).expect("a wxs");

    let stage = opts.out.join("Todo.msi-stage");
    assert_eq!(
        produced.artifact,
        stage.join("Todo.wxs"),
        "staging was asked for, so the source is the artifact"
    );
    assert!(
        produced.missing_tool.is_none(),
        "nothing is missing when nothing was going to be run"
    );
    assert!(
        stage.join("todo").is_file(),
        "the payload is staged beside the source"
    );

    let source = stage.join("Todo.wxs");
    assert!(source.is_file());
    let wxs = fs::read_to_string(&source).expect("the wxs");
    assert!(wxs.contains(r#"UpgradeCode="12345678-1234-1234-1234-123456789012""#));
    assert!(wxs.contains(r#"Version="1.2.3""#));
    assert!(
        wxs.contains(r#"Source="todo""#),
        "an absolute path would only compile on the machine that generated it"
    );
    assert!(
        wxs.contains(r#"Manufacturer="com.example.todo""#),
        "the identifier stands in until one is given"
    );
}

#[test]
fn a_second_run_replaces_the_bundle_rather_than_merging_into_it() {
    let temp = Temp::new("replace");
    let opts = options(&temp, Platform::MacOs);
    let produced = build(&opts).expect("a bundle");

    // Something from a previous build that the current one would not write.
    let stale = produced
        .artifact
        .join("Contents")
        .join("MacOS")
        .join("stale");
    fs::write(&stale, b"left over").expect("a stale file");

    build(&opts).expect("a second bundle");
    assert!(
        !stale.exists(),
        "a merged bundle ships whatever an earlier build left in it"
    );
}
