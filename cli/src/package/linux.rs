//! The Linux `AppDir`, and the `.AppImage` when `appimagetool` is there to seal it.
//!
//! The AppDir is the real output: a self-contained directory with a launcher at a known
//! path, which is a runnable thing in its own right and the documented input to every
//! AppImage tool. Producing it needs nothing but the filesystem, so it happens on any host.
//! `appimagetool` turns it into a single file, needs FUSE and a Linux kernel, and is
//! therefore reported rather than required.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{Error, Options, Produced, clear, install_binary, tool_exists};

/// Builds `<out>/<name>.AppDir`, then `<out>/<name>.AppImage` if the tool is present.
pub fn build(options: &Options) -> Result<Produced, Error> {
    let dir = options.out.join(format!("{}.AppDir", options.name));
    clear(&dir)?;

    let bin = dir.join("usr").join("bin");
    fs::create_dir_all(&bin)?;
    let applications = dir.join("usr").join("share").join("applications");
    fs::create_dir_all(&applications)?;

    let executable = executable_name(options);
    install_binary(&options.binary, &bin.join(&executable))?;

    // The desktop entry is required twice, at the root and under `usr/share`. The root copy
    // is what `appimagetool` reads; the other is what lands in the menu once installed.
    let entry = desktop_entry(options, &executable);
    let entry_name = format!("{}.desktop", options.identifier);
    fs::write(dir.join(&entry_name), &entry)?;
    fs::write(applications.join(&entry_name), &entry)?;

    let run = dir.join("AppRun");
    fs::write(&run, app_run(&executable))?;
    super::set_executable(&run)?;

    if let Some(icon) = &options.icon {
        // Named for the identifier because that is what `Icon=` says. AppImage looks for it
        // at the AppDir root, by that name, with an extension it recognises.
        let extension = icon.extension().unwrap_or_default();
        let mut target = dir.join(&options.identifier);
        target.set_extension(extension);
        fs::copy(icon, &target)?;
    }

    seal(options, &dir)
}

/// Runs `appimagetool` over the AppDir when it is on `PATH`.
fn seal(options: &Options, dir: &Path) -> Result<Produced, Error> {
    if !tool_exists("appimagetool") {
        return Ok(Produced {
            artifact: dir.to_path_buf(),
            missing_tool: Some("appimagetool".to_owned()),
        });
    }
    let image = options.out.join(format!("{}.AppImage", options.name));
    let status = Command::new("appimagetool").arg(dir).arg(&image).status()?;
    if !status.success() {
        return Err(Error::Io(std::io::Error::other(format!(
            "appimagetool failed with {status}"
        ))));
    }
    Ok(Produced {
        artifact: image,
        missing_tool: None,
    })
}

/// The executable's name inside `usr/bin`.
fn executable_name(options: &Options) -> String {
    options.binary.file_name().map_or_else(
        || options.name.clone(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The launcher AppImage runs.
///
/// `readlink -f` rather than `$0`'s directory: AppImage mounts the AppDir somewhere else
/// entirely and the launcher is usually reached through a symlink, so resolving it is the
/// difference between finding the binary and not. `exec` so signals and the exit code belong
/// to the application rather than to a shell that outlived it, and `"$@"` so arguments
/// survive — including ones with spaces, which is why it is quoted.
pub fn app_run(executable: &str) -> String {
    format!(
        "#!/bin/sh\n\
         set -e\n\
         HERE=$(dirname \"$(readlink -f \"$0\")\")\n\
         exec \"$HERE/usr/bin/{executable}\" \"$@\"\n"
    )
}

/// The `.desktop` entry.
///
/// Not XML, so [`super::xml_escape`] would be actively wrong here: the format escapes with
/// backslashes and treats a newline in a value as the end of it.
pub fn desktop_entry(options: &Options, executable: &str) -> String {
    let name = desktop_escape(&options.name);
    let icon = desktop_escape(&options.identifier);
    let executable = desktop_escape(executable);
    let version = desktop_escape(&options.version);
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec={executable}\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-AppVersion={version}\n"
    )
}

/// Escapes the characters a desktop entry value cannot carry literally.
fn desktop_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(character),
        }
    }
    out
}
