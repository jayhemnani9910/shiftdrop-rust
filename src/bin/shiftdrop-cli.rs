//! The command line on its own, as a console program: on Windows `shiftdrop.exe`
//! is a windowed app, which can't print to the terminal that started it.
fn main() -> std::process::ExitCode {
    shiftdrop::cli::main()
}
