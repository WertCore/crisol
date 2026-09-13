//! The Windows installer: a WiX source, and a `.msi` when WiX is there to compile it.
//!
//! The `.wxs` is the real output of this module. It is text, so it is produced on any host
//! and is what CI checks; compiling it needs the WiX toolset, which is Windows-only, so that
//! step runs when the tool is present and is reported when it is not.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{Error, Options, Produced, clear, tool_exists, xml_escape};

/// The largest value each field of an MSI `ProductVersion` may take.
///
/// Not a style rule — Windows Installer packs the version into 32 bits, so a fourth field is
/// ignored outright and anything above these wraps. The failure is silent and specific: the
/// installer builds, installs, and then never recognises the next version as an upgrade, so
/// users end up with two copies. Rejecting it here is the only place it is cheap to catch.
const MAX_MAJOR: u32 = 255;
const MAX_MINOR: u32 = 255;
const MAX_BUILD: u32 = 65_535;

/// Builds `<out>/<name>.wxs` and stages the payload, then compiles when WiX is present.
pub fn build(options: &Options) -> Result<Produced, Error> {
    let stage = options.out.join(format!("{}.msi-stage", options.name));
    clear(&stage)?;
    fs::create_dir_all(&stage)?;

    let executable = executable_name(options);
    // Staged beside the `.wxs` so that `Source=` is a bare file name and the source is
    // relocatable: an absolute path baked into it would only compile on this machine.
    super::install_binary(&options.binary, &stage.join(&executable))?;
    if let Some(icon) = &options.icon {
        fs::copy(icon, stage.join("icon.ico"))?;
    }

    let source = stage.join(format!("{}.wxs", options.name));
    fs::write(&source, wxs(options, &executable)?)?;

    seal(options, &stage, &source)
}

/// Compiles the `.wxs` with whichever WiX is installed.
fn seal(options: &Options, stage: &Path, source: &Path) -> Result<Produced, Error> {
    // Asked not to, so the source is the answer rather than a fallback.
    if !options.seal {
        return Ok(Produced {
            artifact: source.to_path_buf(),
            missing_tool: None,
        });
    }
    let msi = options.out.join(format!("{}.msi", options.name));
    // WiX 4 and 5 ship a single `wix` driver; WiX 3 ships `candle` and `light`. Both are
    // still in wide use, so both are accepted rather than picking one and calling the other
    // unsupported.
    let status = if tool_exists("wix") {
        Command::new("wix")
            .args(["build", "-o"])
            .arg(&msi)
            .arg(source)
            .current_dir(stage)
            .status()?
    } else if tool_exists("candle") && tool_exists("light") {
        let object = stage.join("product.wixobj");
        let candle = Command::new("candle")
            .arg("-o")
            .arg(&object)
            .arg(source)
            .current_dir(stage)
            .status()?;
        if !candle.success() {
            return Err(Error::Io(std::io::Error::other(format!(
                "candle failed with {candle}"
            ))));
        }
        Command::new("light")
            .arg("-o")
            .arg(&msi)
            .arg(&object)
            .current_dir(stage)
            .status()?
    } else {
        return Ok(Produced {
            artifact: source.to_path_buf(),
            missing_tool: Some("wix (or candle and light)".to_owned()),
        });
    };
    if !status.success() {
        return Err(Error::Io(std::io::Error::other(format!(
            "WiX failed with {status}"
        ))));
    }
    Ok(Produced {
        artifact: msi,
        missing_tool: None,
    })
}

/// The executable's name inside the install folder.
fn executable_name(options: &Options) -> String {
    options.binary.file_name().map_or_else(
        || options.name.clone(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Rejects a version Windows Installer cannot represent.
///
/// # Errors
///
/// [`Error::BadVersion`] naming the field that is out of range.
pub fn check_version(version: &str) -> Result<(), Error> {
    super::check_dotted_version(version)?;
    let mut fields = version.split('.');
    let mut next = |name: &str, max: u32| -> Result<(), Error> {
        let Some(field) = fields.next() else {
            return Ok(());
        };
        let value: u32 = field.parse().map_err(|_| {
            Error::BadVersion(format!("{field:?} is too large to be an MSI {name} field"))
        })?;
        if value > max {
            return Err(Error::BadVersion(format!(
                "an MSI {name} field is at most {max}, and this one is {value}.\n\
                 Windows Installer packs the version into 32 bits: a larger value wraps, and\n\
                 the installer then fails to recognise the next version as an upgrade."
            )));
        }
        Ok(())
    };
    next("major", MAX_MAJOR)?;
    next("minor", MAX_MINOR)?;
    next("build", MAX_BUILD)?;
    if fields.next().is_some() {
        return Err(Error::BadVersion(format!(
            "{version:?} has more than three fields. Windows Installer ignores the fourth\n\
             when comparing versions, so two releases differing only there are the same\n\
             release as far as upgrades are concerned."
        )));
    }
    Ok(())
}

/// The WiX source.
///
/// # Errors
///
/// [`Error::MissingUpgradeCode`] when no `UpgradeCode` was supplied.
pub fn wxs(options: &Options, executable: &str) -> Result<String, Error> {
    let upgrade = options
        .upgrade_code
        .as_deref()
        .ok_or(Error::MissingUpgradeCode)?;
    let name = xml_escape(&options.name);
    let manufacturer = xml_escape(
        options
            .manufacturer
            .as_deref()
            .unwrap_or(&options.identifier),
    );
    let version = xml_escape(&options.version);
    let upgrade = xml_escape(upgrade);
    let executable = xml_escape(executable);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Generated by `crisol package`. -->
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*"
           Name="{name}"
           Language="1033"
           Version="{version}"
           Manufacturer="{manufacturer}"
           UpgradeCode="{upgrade}">
    <Package InstallerVersion="200" Compressed="yes" InstallScope="perMachine" />
    <MajorUpgrade DowngradeErrorMessage="A newer version of {name} is already installed." />
    <MediaTemplate EmbedCab="yes" />

    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="ProgramFiles64Folder">
        <Directory Id="INSTALLFOLDER" Name="{name}" />
      </Directory>
    </Directory>

    <ComponentGroup Id="ApplicationFiles" Directory="INSTALLFOLDER">
      <Component Id="MainExecutable" Guid="*">
        <File Id="MainExecutableFile" Source="{executable}" KeyPath="yes" />
      </Component>
    </ComponentGroup>

    <Feature Id="Complete" Title="{name}" Level="1">
      <ComponentGroupRef Id="ApplicationFiles" />
    </Feature>
  </Product>
</Wix>
"#
    ))
}
