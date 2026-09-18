//! The compiler driver.
//!
//! `crisol build` is a thin wrapper over [`build`], and the split exists so the acceptance can
//! be tested by *calling* the pipeline rather than by spawning the CLI. A subprocess test would
//! depend on the `crisol` binary already being built, which is the same ordering problem as the
//! runtime archive and would make the test depend on how `cargo test` schedules its work.

#![doc(html_root_url = "https://docs.rs/crisol/0.0.0")]

pub mod build;
