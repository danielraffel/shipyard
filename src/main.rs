//! Binary entrypoint for Shipyard.

use std::process::ExitCode;

fn main() -> ExitCode {
    shipyard::app::run()
}
