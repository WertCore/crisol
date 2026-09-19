//! `crisol build` — source to a native binary.
//!
//! §M13's acceptance is that a program "compiles to a standalone binary that runs and produces
//! correct output". This is the path that claim rests on, and it has four steps that can each
//! fail differently: parse, lower, compile, link.
//!
//! # Linking needs a C entry point
//!
//! The generated object exports the program as a function; something has to call it. That
//! something is a five-line C `main`, written to a temporary file and compiled by the host's
//! `cc`, rather than a Rust one — a Rust `main` would drag in `std`'s runtime initialisation
//! and make the binary's contents depend on a Rust version rather than on what was compiled.
//!
//! It calls the runtime's `crisol_print` rather than decoding the value itself, because the
//! NaN-box layout is the runtime's business and putting a copy of it in generated C would be a
//! second place for it to drift.

use std::path::{Path, PathBuf};
use std::process::Command;

use crisol_codegen::{Backend as _, Cranelift};

/// Why a build stopped.
///
/// Each variant is a different *stage*, because "it did not build" is not an actionable
/// message — the interesting part is always which of parse, lower, compile or link gave up.
#[derive(Debug)]
pub enum BuildError {
    /// The source could not be read.
    Unreadable {
        /// Which file.
        path: PathBuf,
        /// What the filesystem said.
        message: String,
    },
    /// The source did not parse.
    Parse {
        /// What the parser said.
        messages: Vec<String>,
    },
    /// The source parsed but used something the lowering does not handle.
    ///
    /// Separate from [`BuildError::Parse`] because the fix is entirely different: a parse error
    /// is a mistake in the program, and this is a gap in the compiler.
    Unsupported {
        /// What it was.
        constructs: Vec<String>,
    },
    /// The IR was malformed, which is a bug here rather than in the program.
    Malformed {
        /// What the verifier said.
        message: String,
    },
    /// Code generation refused something.
    Codegen {
        /// What it said.
        message: String,
    },
    /// The link step failed.
    Link {
        /// What the linker said.
        message: String,
    },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, message } => {
                write!(f, "cannot read {}: {message}", path.display())
            }
            Self::Parse { messages } => {
                write!(f, "the source did not parse:\n  {}", messages.join("\n  "))
            }
            Self::Unsupported { constructs } => write!(
                f,
                "the compiler does not handle these yet:\n  {}",
                constructs.join("\n  ")
            ),
            Self::Malformed { message } => {
                write!(
                    f,
                    "produced malformed IR, which is a bug in crisol: {message}"
                )
            }
            Self::Codegen { message } => write!(f, "cannot generate code: {message}"),
            Self::Link { message } => write!(f, "cannot link: {message}"),
        }
    }
}

/// Compiles `source` to a native executable at `output`.
///
/// `runtime` is the static library providing the ABI the generated code calls.
///
/// # Errors
///
/// [`BuildError`], naming the stage that stopped.
pub fn build(source: &Path, output: &Path, runtime: &Path) -> Result<(), BuildError> {
    let text = std::fs::read_to_string(source).map_err(|error| BuildError::Unreadable {
        path: source.to_path_buf(),
        message: error.to_string(),
    })?;

    let lowered =
        crisol_frontend::lower("crisol_program", &text).map_err(|error| BuildError::Parse {
            messages: error.errors,
        })?;

    if !lowered.is_faithful() {
        // Refused rather than compiled, because a program that silently omits what the
        // compiler did not understand is worse than one that does not build (D-59).
        return Err(BuildError::Unsupported {
            constructs: lowered
                .unsupported
                .iter()
                .map(|note| format!("{} (byte {})", note.what, note.at))
                .collect(),
        });
    }

    crisol_ir::verify_module(&lowered.functions).map_err(|errors| BuildError::Malformed {
        message: errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
    })?;

    let mut backend = Cranelift::new(host_triple()).map_err(|error| BuildError::Codegen {
        message: error.to_string(),
    })?;
    for function in &lowered.functions {
        backend
            .compile(function)
            .map_err(|error| BuildError::Codegen {
                message: format!("{}: {error}", function.name),
            })?;
    }
    let object = backend.finish().map_err(|error| BuildError::Codegen {
        message: error.to_string(),
    })?;

    link(&object, output, runtime)
}

/// The triple this build is running on.
const fn host_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-pc-windows-msvc"
    }
}

/// Links the object, a C entry point and the runtime into an executable.
fn link(object: &[u8], output: &Path, runtime: &Path) -> Result<(), BuildError> {
    let directory = output.parent().unwrap_or(Path::new(".")).to_path_buf();
    let object_path = directory.join("crisol-program.o");
    let entry_path = directory.join("crisol-entry.c");

    std::fs::write(&object_path, object).map_err(|error| BuildError::Link {
        message: format!("cannot write the object file: {error}"),
    })?;
    std::fs::write(
        &entry_path,
        // The program hands its stack map table to the runtime before running anything. The
        // runtime cannot look the symbol up itself: an `extern` reference in `crisol-abi`
        // would make that crate fail to link anywhere the symbol does not exist, including
        // its own tests.
        //
        // Element 0 is the row count and the rows begin at element 1 — the layout the backend
        // writes (D-90). Registering *before* `crisol_program` runs is the whole point: a
        // collection can happen on the first allocation.
        format!(
            // `crisol_program` takes the five operands every compiled function takes
            // (closure, this, new.target, argc, argv), because a call site cannot know which
            // function it is reaching. The program itself is called with none: no closure, no
            // `this` yet, no `new.target`, no arguments.
            //
            // `argv` still points at a real slot. A parameter is read under a select rather
            // than a branch, so the load happens even for an argument that was not passed and
            // has to be in bounds — `ARGV_MIN_SLOTS` is that guarantee, and this honours it.
            //
            // The `undefined` bit pattern is interpolated from the Rust constant rather than
            // written out here, so the NaN-box layout stays in one place.
            "extern unsigned long long crisol_stack_maps[];\n\
             extern unsigned long long crisol_functions[];\n\
             extern void crisol_register_functions(const void *table, unsigned long long count);\n\
             extern void crisol_register_stack_maps(const void *rows, unsigned long long count);\n\
             extern unsigned long long crisol_program(unsigned long long closure,\n\
                 unsigned long long this_value, unsigned long long new_target,\n\
                 unsigned long long argc, unsigned long long *argv);\n\
             extern void crisol_print(unsigned long long);\n\
             extern void crisol_report_uncaught(void);\n\
             int main(void) {{\n\
                 unsigned long long argv[{slots}] = {{ {undefined}ULL }};\n\
                 crisol_register_stack_maps(&crisol_stack_maps[1], crisol_stack_maps[0]);\n\
                 crisol_register_functions(&crisol_functions[1], crisol_functions[0]);\n\
                 unsigned long long result =\n\
                     crisol_program(0ULL, {undefined}ULL, {undefined}ULL, 0ULL, argv);\n\
                 if (result == {exception}ULL) {{\n\
                     crisol_report_uncaught();\n\
                     return 1;\n\
                 }}\n\
                 crisol_print(result);\n\
                 return 0;\n\
             }}\n",
            undefined = crisol_value::Value::UNDEFINED.to_bits(),
            slots = crisol_codegen::ARGV_MIN_SLOTS,
            exception = crisol_value::Value::EXCEPTION.to_bits(),
        ),
    )
    .map_err(|error| BuildError::Link {
        message: format!("cannot write the entry point: {error}"),
    })?;

    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let result = Command::new(&compiler)
        .arg(&entry_path)
        .arg(&object_path)
        .arg(runtime)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|error| BuildError::Link {
            message: format!("cannot run {compiler}: {error}"),
        })?;

    if !result.status.success() {
        return Err(BuildError::Link {
            message: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        });
    }
    Ok(())
}
