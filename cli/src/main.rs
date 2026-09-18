//! The Crisol command line.
//!
//! Most of these subcommands are not implemented yet. They exist anyway, and they say which
//! milestone implements them, because a command that does not exist is indistinguishable
//! from a typo and a command that silently succeeds without doing anything is worse than
//! either.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod doctor;
mod package;

/// Exit code for a command that is real but not implemented yet.
///
/// Distinct from 1, which means the command ran and failed. A script can tell the
/// difference between "this build is broken" and "this version of crisol cannot do that".
const EXIT_UNIMPLEMENTED: u8 = 2;

#[derive(Parser, Debug)]
#[command(
    name = "crisol",
    version,
    about = "Run HTML/CSS/JS applications as native binaries",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
// Only Windows trips this: `PathBuf` is 32 bytes there rather than 24, which is enough to
// push `Package` over the threshold. Boxing it would fight clap's derive, and the cost the
// lint is about — carrying the widest variant everywhere — is not paid here, because a
// command is parsed once, matched once, and dropped.
#[allow(
    clippy::large_enum_variant,
    reason = "parsed once, matched once, then dropped"
)]
enum Command {
    /// Compile a project to a native binary.
    Build {
        /// Project directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Run a project with the interpreter and hot reload.
    Dev {
        /// Project directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Build and run a project.
    Run {
        /// Project directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Type-check and resolve a project without compiling it.
    Check {
        /// Project directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Wrap a built binary into the artifact its platform expects.
    ///
    /// Takes an executable rather than a project: `crisol build` is M13, and until it
    /// exists the input is a Rust application built against `crisol-ui`, which is what
    /// ROADMAP §M8's acceptance describes. The same command takes `build`'s output later.
    Package(package::Request),

    /// Report what this machine and this build of crisol can do.
    Doctor,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Doctor => {
            doctor::report();
            ExitCode::SUCCESS
        }
        Command::Build { path } => build_command(&path),
        Command::Dev { .. } => unimplemented("dev", "M17", "React on the dev-mode interpreter"),
        Command::Run { .. } => unimplemented("run", "M16", "the DOM host API"),
        Command::Check { .. } => unimplemented("check", "M10", "the frontend and module graph"),
        Command::Package(request) => package(request),
    }
}

/// Runs `crisol build` and reports what came of it.
///
/// The runtime archive is located next to the running executable, which is where a `cargo
/// build` of this workspace puts it. An installed toolchain would ship it alongside the binary
/// for the same reason, so the lookup is the same in both cases.
fn build_command(source: &std::path::Path) -> ExitCode {
    let output = source.with_extension("");
    let runtime = match runtime_archive() {
        Some(path) => path,
        None => {
            eprintln!(
                "cannot find the runtime archive (libcrisol_abi.a) next to this executable.\n\
                 A compiled program links against it, so building needs it present."
            );
            return ExitCode::FAILURE;
        }
    };

    match crisol::build::build(source, &output, &runtime) {
        Ok(()) => {
            println!("built {}", output.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Where the runtime archive lives.
fn runtime_archive() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let candidate = directory.join("libcrisol_abi.a");
    candidate.is_file().then_some(candidate)
}

/// Runs `crisol package` and reports what came of it.
fn package(request: package::Request) -> ExitCode {
    let options = match request.resolve() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("crisol package: {error}");
            return ExitCode::FAILURE;
        }
    };
    match package::build(&options) {
        Ok(produced) => {
            println!("{}", produced.artifact.display());
            if let Some(tool) = produced.missing_tool {
                // Loud, and on stderr, because the caller asked for a sealed artifact and
                // got the thing that goes into one. Still a success: the layout is complete
                // and correct, and it is the documented input to the tool that is missing.
                eprintln!();
                eprintln!("note: {tool} is not on PATH, so the layout above was not sealed.");
                eprintln!("      It is complete, and is what that tool takes as input.");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("crisol package: {error}");
            ExitCode::FAILURE
        }
    }
}

fn unimplemented(command: &str, milestone: &str, needs: &str) -> ExitCode {
    eprintln!("crisol {command}: not implemented yet.");
    eprintln!();
    eprintln!("  Scheduled for {milestone}, which needs {needs}.");
    eprintln!("  Run `crisol doctor` to see what is implemented, or read ROADMAP.md.");
    ExitCode::from(EXIT_UNIMPLEMENTED)
}
