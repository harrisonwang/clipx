fn main() {
    if let Err(error) = clipx::run_cli() {
        eprintln!("clipx：{error:#}");
        std::process::exit(1);
    }
}
