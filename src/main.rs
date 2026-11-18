use clap::{Parser, error::ErrorKind};
use cpforge::{cli::Cli, error::Error, exit::ExitCode};

fn main() -> std::process::ExitCode {
    if let Err(error) = cpforge::supervisor::init_process() {
        eprintln!("cpf: error: cannot mark inherited descriptors close-on-exec: {error}");
        return ExitCode::Internal.into();
    }
    let result = Cli::try_parse().map_err(Error::from).and_then(cpforge::run);
    match result {
        Ok(()) => ExitCode::Success.into(),
        Err(error) => {
            let code = error.exit_code();
            match error {
                Error::Cli(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::DisplayHelp
                            | ErrorKind::DisplayVersion
                            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    ) =>
                {
                    if let Err(error) = error.print() {
                        eprintln!("cpf: error: {error}");
                        return ExitCode::Internal.into();
                    }
                }
                error => eprintln!("cpf: error: {error}"),
            }
            code.into()
        }
    }
}
