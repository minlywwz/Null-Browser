#![windows_subsystem = "windows"]

fn main() {
    if let Err(error) = web_content_process::serve() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
