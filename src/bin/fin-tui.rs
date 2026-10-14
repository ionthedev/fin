fn main() {
    if let Err(error) = fin::tui_cli::run(std::env::args_os()) {
        eprintln!("fin-tui: {error}");
        std::process::exit(1);
    }
}
