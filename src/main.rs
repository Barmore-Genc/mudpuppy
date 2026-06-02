//! The `mudpuppy` binary: a thin shell around the [`mudpuppy`] library. It
//! parses the clap command tree and dispatches; all real logic lives in the
//! library so it can be tested directly.

use std::process::ExitCode;

fn main() -> ExitCode {
    // Off by default; `MUDPUPPY_LOG=<path>` opens a debug log. A failed open is
    // non-fatal — report it before the TUI takes over the terminal and carry on
    // with logging disabled.
    if let Some(path) = std::env::var_os("MUDPUPPY_LOG") {
        if let Err(err) = mudpuppy::logging::init_file(std::path::Path::new(&path)) {
            eprintln!("warning: could not open MUDPUPPY_LOG file: {err}");
        }
    }

    if let Err(err) = mudpuppy::cli::run() {
        // `{:#}` renders the full anyhow context chain on one line.
        eprintln!("error: {err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
