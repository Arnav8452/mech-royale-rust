mod bot;
mod io;
mod map;
mod nav;
mod protocol;

fn main() {
    if let Err(e) = io::run() {
        eprintln!("fatal bot error: {e}");
        std::process::exit(1);
    }
}
