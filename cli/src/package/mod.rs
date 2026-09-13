//! `crisol package` — wrap a built binary into the artifact a platform expects to be handed.
//!
//! At M8 the input is a native executable that already exists: a Rust application built
//! against `crisol-ui`, which is what ROADMAP §M8's acceptance describes ("built entirely in
//! Rust"). From M13 `crisol build` produces that executable from an HTML/CSS/JS project and
//! this module is handed its output instead. Nothing here assumes which of the two made it,
//! which is why it takes a path rather than a project.
//!
//! **The layout is built anywhere; only the container is host-gated.** A `.app` is a
//! directory, an AppDir is a directory, and a `.wxs` is text — all three can be produced on
//! any host, and are, because CI runs on three platforms and a packager that can only be
//! tested on one is a packager that is broken on two. Turning an AppDir into a `.AppImage` or
//! a `.wxs` into a `.msi` needs `appimagetool` and WiX respectively, so those steps run when
//! the tool is there and say plainly what is missing when it is not.

use clap::Args;

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

mod linux;
mod macos;
mod windows;

#[cfg(test)]
mod tests;

/// Which platform's artifact to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// A `.app` bundle.
    MacOs,
    /// A `.wxs` source, and a `.msi` when WiX is present.
    Windows,
    /// An `AppDir`, and a `.AppImage` when `appimagetool` is present.
    Linux,
}

impl Platform {
    /// The platform this binary is running on, which is the default target.
    pub fn host() -> Option<Self> {
        match std::env::consts::OS {
            "macos" => Some(Self::MacOs),
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    /// The name accepted on the command line.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "macos" => Some(Self::MacOs),
            "windows" => Some(Self::Windows),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        })
    }
}

/// Everything a bundle needs that the binary itself cannot say.
#[derive(Debug, Clone)]
pub struct Options {
    /// The executable to wrap.
    pub binary: PathBuf,
    /// The name a user sees. Also the bundle's own file name.
    pub name: String,
    /// Reverse-DNS identifier. macOS requires one; the others use it for their own ids.
    pub identifier: String,
    /// The application's version, not crisol's.
    pub version: String,
    /// Directory the artifact is written into.
    pub out: PathBuf,
    /// Which platform to build for.
    pub target: Platform,
    /// An icon, already in the format the target wants: `.icns`, `.png`, `.ico`. Not
    /// converted — a packager that re-encodes images is a packager that owns an image
    /// library, and the platforms disagree about sizes and colour profiles in ways a
    /// generic conversion gets wrong.
    pub icon: Option<PathBuf>,
    /// The name Windows shows as the publisher. Defaults to the identifier, which is at
    /// least stable and traceable, and is overridable because it is also ugly.
    pub manufacturer: Option<String>,
    /// MSI `UpgradeCode`. Required for Windows, and deliberately not invented — see
    /// [`Error::MissingUpgradeCode`].
    pub upgrade_code: Option<String>,
}

/// The command line's arguments, before defaults are worked out.
///
/// Separate from [`Options`] so that defaulting is one place with one set of rules, and so
/// that `Options` is always a fully-decided description of a bundle rather than a bag of
/// maybes each consumer has to re-interpret.
///
/// The arguments are declared here rather than on the subcommand so that there is one list
/// of them. Declared in both places, a field added to one and forgotten in the other
/// compiles perfectly and silently ignores the flag.
#[derive(Args, Debug, Clone)]
pub struct Request {
    /// The executable to wrap.
    #[arg(long = "bin", value_name = "FILE")]
    pub binary: PathBuf,
    /// The name a user sees. Defaults to the executable's file stem.
    #[arg(long)]
    pub name: Option<String>,
    /// Reverse-DNS bundle identifier, like com.example.todo.
    #[arg(long)]
    pub identifier: String,
    /// The application's version, not crisol's.
    #[arg(long, default_value = "0.1.0")]
    pub version: String,
    /// Directory to write the artifact into.
    #[arg(long, default_value = "dist")]
    pub out: PathBuf,
    /// Platform to build for: macos, windows or linux. Defaults to this machine.
    #[arg(long)]
    pub target: Option<String>,
    /// An icon, already in the format the target wants (.icns, .png, .ico).
    #[arg(long, value_name = "FILE")]
    pub icon: Option<PathBuf>,
    /// The publisher name Windows shows. Defaults to the identifier.
    #[arg(long)]
    pub manufacturer: Option<String>,
    /// MSI UpgradeCode GUID. Required when targeting Windows, and never invented.
    #[arg(long, value_name = "GUID")]
    pub upgrade_code: Option<String>,
}

impl Request {
    /// Applies the defaults.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownTarget`] for a platform name that is not one of the three, and
    /// [`Error::UnknownHost`] when no target was named and this machine is not one either.
    pub fn resolve(self) -> Result<Options, Error> {
        let target = match &self.target {
            Some(name) => {
                Platform::parse(name).ok_or_else(|| Error::UnknownTarget(name.clone()))?
            }
            None => Platform::host().ok_or(Error::UnknownHost)?,
        };
        let name = match self.name {
            Some(name) => name,
            // The stem, not the file name: `todo.exe` should not produce `todo.exe.app`.
            None => self
                .binary
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .ok_or_else(|| Error::BadName(self.binary.display().to_string()))?,
        };
        Ok(Options {
            binary: self.binary,
            name,
            identifier: self.identifier,
            version: self.version,
            out: self.out,
            target,
            icon: self.icon,
            manufacturer: self.manufacturer,
            upgrade_code: self.upgrade_code,
        })
    }
}

/// Why a package could not be produced.
#[derive(Debug)]
pub enum Error {
    /// A `--target` that is not one of the three.
    UnknownTarget(String),
    /// No `--target`, and this machine is not one of the three either.
    UnknownHost,
    /// The named executable is not there.
    NoBinary(PathBuf),
    /// The name is empty, or holds a path separator, so it cannot be a file name.
    BadName(String),
    /// A bundle identifier has to look like `com.example.app`.
    BadIdentifier(String),
    /// The version is not in a form the target accepts. Carries what was wrong.
    BadVersion(String),
    /// Windows needs a stable `UpgradeCode` GUID and this tool will not make one up: it has
    /// to stay identical across every version ever shipped, or upgrades install alongside
    /// rather than replace. That is a decision about an application's identity, and a
    /// packager that guessed would quietly break the first upgrade.
    MissingUpgradeCode,
    /// Something underneath failed.
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTarget(name) => {
                write!(
                    f,
                    "{name:?} is not a target: expected macos, windows or linux"
                )
            }
            Self::UnknownHost => f.write_str(
                "this machine is not a platform crisol packages for, so --target is required",
            ),
            Self::NoBinary(path) => write!(f, "no such executable: {}", path.display()),
            Self::BadName(name) => {
                write!(
                    f,
                    "{name:?} cannot be a bundle name: it must be non-empty and hold no path separator"
                )
            }
            Self::BadIdentifier(id) => write!(
                f,
                "{id:?} is not a bundle identifier: expected reverse-DNS, like com.example.app"
            ),
            Self::BadVersion(why) => write!(f, "{why}"),
            Self::MissingUpgradeCode => f.write_str(
                "packaging for Windows needs --upgrade-code <GUID>.\n\
                 \n\
                 It has to be the same GUID for every version this application ever ships, so\n\
                 that an installer replaces the previous one instead of installing beside it.\n\
                 Generate one once, keep it, and pass it every time. crisol will not invent it.",
            ),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

/// What was produced, and what could not be.
#[derive(Debug)]
pub struct Produced {
    /// The artifact, or the staged directory when the container step could not run.
    pub artifact: PathBuf,
    /// Set when the layout is complete but the platform tool that seals it is absent.
    pub missing_tool: Option<String>,
}

/// Builds the artifact described by `options`.
///
/// # Errors
///
/// Returns [`Error`] when the inputs do not describe a bundle that could exist, or when the
/// filesystem refuses.
pub fn build(options: &Options) -> Result<Produced, Error> {
    validate(options)?;
    fs::create_dir_all(&options.out)?;
    match options.target {
        Platform::MacOs => macos::build(options),
        Platform::Linux => linux::build(options),
        Platform::Windows => windows::build(options),
    }
}

/// Rejects inputs that could not produce a valid bundle, before anything is written.
///
/// Up front on purpose: half a bundle on disk is worse than none, because it looks like a
/// build that worked.
fn validate(options: &Options) -> Result<(), Error> {
    if !options.binary.is_file() {
        return Err(Error::NoBinary(options.binary.clone()));
    }
    if options.name.is_empty()
        || options.name.contains('/')
        || options.name.contains('\\')
        || options.name.contains('\0')
    {
        return Err(Error::BadName(options.name.clone()));
    }
    if !is_reverse_dns(&options.identifier) {
        return Err(Error::BadIdentifier(options.identifier.clone()));
    }
    match options.target {
        Platform::Windows => {
            windows::check_version(&options.version)?;
            if options.upgrade_code.is_none() {
                return Err(Error::MissingUpgradeCode);
            }
        }
        Platform::MacOs | Platform::Linux => check_dotted_version(&options.version)?,
    }
    Ok(())
}

/// Whether `id` is a reverse-DNS identifier: two or more dot-separated segments, each of
/// which is alphanumeric or a hyphen and does not start or end with one.
fn is_reverse_dns(id: &str) -> bool {
    let mut segments = 0;
    for segment in id.split('.') {
        segments += 1;
        if segment.is_empty()
            || segment.starts_with('-')
            || segment.ends_with('-')
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return false;
        }
    }
    segments >= 2
}

/// Accepts `1`, `1.2`, `1.2.3`, … of plain non-negative integers.
///
/// # Errors
///
/// [`Error::BadVersion`] describing what was wrong.
fn check_dotted_version(version: &str) -> Result<(), Error> {
    if version.is_empty() {
        return Err(Error::BadVersion("the version is empty".to_owned()));
    }
    for part in version.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::BadVersion(format!(
                "{version:?} is not a version: expected dot-separated numbers, like 1.2.3"
            )));
        }
    }
    Ok(())
}

/// Escapes the five characters XML cannot carry literally.
///
/// Both the plist and the `.wxs` are XML, and both interpolate a name the user chose. An
/// application called `Tom & Jerry` produces a file no parser will read without this.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

/// Copies the executable to `destination` and makes it executable where that is a concept.
fn install_binary(from: &Path, to: &Path) -> io::Result<()> {
    fs::copy(from, to)?;
    set_executable(to)
}

/// Sets the executable bit on Unix.
///
/// On Windows there is no bit to set, and a `.app` staged there will not run on macOS until
/// it has crossed a filesystem that has one. Saying so is better than pretending: the archive
/// step, not this one, is where that is usually fixed.
#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes `path` if it is there, so a second run replaces rather than merges.
///
/// Merging is the subtle failure: a rename that drops a file leaves the old one behind in the
/// bundle, and the result runs locally and ships something nobody meant to ship.
fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether an external tool is on `PATH`.
fn tool_exists(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}
