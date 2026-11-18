//! Test-only scripts and a process containment harness. See the crate README for
//! the control protocol, binary discovery, and script escaping.

pub mod harness;
mod platform;
mod script;

/// Run a validated fixture script, returning a diagnostic exit code on failure.
pub fn run(args: &[String]) -> i32 {
    if let Err(error) = platform::startup() {
        eprintln!("cpf-fixture: {error}");
        return 65;
    }
    let steps = match script::parse(args, 0) {
        Ok(steps) => steps,
        Err(error) => {
            eprintln!("cpf-fixture: {error}");
            return 64;
        }
    };
    match script::execute(steps) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cpf-fixture: {error}");
            if error.kind() == std::io::ErrorKind::TimedOut {
                65
            } else {
                70
            }
        }
    }
}
