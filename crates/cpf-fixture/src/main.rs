fn main() {
    let args: Result<Vec<_>, _> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect();
    let args = match args {
        Ok(args) => args,
        Err(_) => {
            eprintln!("cpf-fixture: steps must be valid UTF-8");
            std::process::exit(64);
        }
    };
    std::process::exit(cpf_fixture::run(&args));
}
