//! The Crisol command line.
//!
//! Most of these subcommands are not implemented yet. They exist anyway, and they say which
//! milestone implements them, because a command that does not exist is indistinguishable
//! from a typo and a command that silently succeeds without doing anything is worse than
//! either.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod doctor;

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
    /// Produce a distributable artifact.
    Package {
        /// Project directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
    },
    /// Report what this machine and this build of crisol can do.
    Doctor,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Doctor => {
            doctor::report();
            ExitCode::SUCCESS
        }
        Command::Build { .. } => unimplemented("build", "M13", "codegen"),
        Command::Dev { .. } => unimplemented("dev", "M17", "React on the dev-mode interpreter"),
        Command::Run { .. } => unimplemented("run", "M16", "the DOM host API"),
        Command::Check { .. } => unimplemented("check", "M10", "the frontend and module graph"),
        Command::Package { .. } => unimplemented("package", "M8", "platform packaging"),
    }
}

fn unimplemented(command: &str, milestone: &str, needs: &str) -> ExitCode {
    eprintln!("crisol {command}: not implemented yet.");
    eprintln!();
    eprintln!("  Scheduled for {milestone}, which needs {needs}.");
    eprintln!("  Run `crisol doctor` to see what is implemented, or read ROADMAP.md.");
    ExitCode::from(EXIT_UNIMPLEMENTED)
}
