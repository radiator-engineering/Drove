use std::process::ExitCode;

fn main() -> ExitCode {
    match drove::cli::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
