fn main() {
    if let Err(error) = pisci_desktop_lib::headless_cli::run_from_env_args() {
        if !error.is_empty() {
            eprintln!("{}", error);
        }
        std::process::exit(1);
    }
}
