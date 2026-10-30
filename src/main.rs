//! shiftdrop                          run the app (the extension starts it on demand)
//! shiftdrop FILE...                  open the wheel for these files, pick by clicking
//! shiftdrop convert --to FMT FILE...
//! shiftdrop tool NAME FILE... [--start S --end E] [--edge PX]
//! shiftdrop targets FILE...          what this machine can turn FILE into
#[cfg(target_os = "linux")]
mod app;
#[cfg(target_os = "linux")]
mod wheel;

use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    if shiftdrop::cli::wanted(&argv) {
        return shiftdrop::cli::main();
    }
    ExitCode::from(app::main(&argv).get() as u8)
}
