//! The macOS `.app` bundle.
//!
//! A `.app` is a directory with a fixed shape and one required file. Nothing here shells out:
//! the whole artifact is directories, a copy and a plist, so it can be built — and tested —
//! on any host.
//!
//! Not done here: code signing and notarisation. Both need credentials and Apple's tooling,
//! both are refused rather than half-done, and an unsigned `.app` still runs locally and is
//! still the right input to `codesign`. M21 is where shipping to other people lives.

use std::fs;

use super::{Error, Options, Produced, clear, install_binary, xml_escape};

/// Builds `<out>/<name>.app`.
pub fn build(options: &Options) -> Result<Produced, Error> {
    let bundle = options.out.join(format!("{}.app", options.name));
    clear(&bundle)?;

    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    fs::create_dir_all(&macos)?;
    fs::create_dir_all(contents.join("Resources"))?;

    let executable = executable_name(options);
    install_binary(&options.binary, &macos.join(&executable))?;

    let icon = options
        .icon
        .as_ref()
        .map(|icon| -> Result<String, Error> {
            // `CFBundleIconFile` names a file in `Resources`, conventionally without its
            // extension. Copied under the bundle's own name so the plist can name it without
            // depending on what the source file happened to be called.
            let name = format!("{}.icns", options.name);
            fs::copy(icon, contents.join("Resources").join(&name))?;
            Ok(name)
        })
        .transpose()?;

    fs::write(
        contents.join("Info.plist"),
        info_plist(options, &executable, icon.as_deref()),
    )?;

    // Eight bytes that predate the plist. Modern macOS reads the plist, but enough tooling
    // still stats this file that leaving it out causes odd, hard-to-attribute behaviour.
    fs::write(contents.join("PkgInfo"), "APPL????")?;

    Ok(Produced {
        artifact: bundle,
        missing_tool: None,
    })
}

/// The file name the executable takes inside `Contents/MacOS`.
///
/// The binary's own name, not the bundle's: `CFBundleExecutable` has to match it exactly, and
/// a bundle name is free text that may hold spaces or a slash-free but still awkward string.
/// Taking the name of the file being copied means the two can never disagree.
fn executable_name(options: &Options) -> String {
    options.binary.file_name().map_or_else(
        || options.name.clone(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The `Info.plist`, which is the only file macOS truly requires.
///
/// `NSHighResolutionCapable` matters more here than in most bundles. Without it macOS runs
/// the process in its low-resolution compatibility mode and scales the window up afterwards,
/// so a GPU renderer that is drawing correct pixels still ends up visibly blurry on every
/// Retina display — and the engine has no way to detect that it is happening.
pub fn info_plist(options: &Options, executable: &str, icon: Option<&str>) -> String {
    let name = xml_escape(&options.name);
    let identifier = xml_escape(&options.identifier);
    let version = xml_escape(&options.version);
    let executable = xml_escape(executable);
    let icon = icon.map_or_else(String::new, |icon| {
        format!(
            "    <key>CFBundleIconFile</key>\n    <string>{}</string>\n",
            xml_escape(icon)
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleName</key>
    <string>{name}</string>
    <key>CFBundleDisplayName</key>
    <string>{name}</string>
    <key>CFBundleIdentifier</key>
    <string>{identifier}</string>
    <key>CFBundleExecutable</key>
    <string>{executable}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
{icon}</dict>
</plist>
"#
    )
}
