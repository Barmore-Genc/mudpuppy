//! The `mudpuppy` binary: a thin shell around the [`mudpuppy`] library. It
//! parses the clap command tree and dispatches; all real logic lives in the
//! library so it can be tested directly.

use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(err) = mudpuppy::cli::run() {
        // `{:#}` renders the full anyhow context chain on one line.
        eprintln!("error: {err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
