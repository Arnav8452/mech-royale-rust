mod bot;
mod combat;
mod geom;
mod io;
mod map;
mod nav;
mod protocol;
mod steering;
mod strategy;
mod world;

fn main() {
    if let Err(e) = io::run() {
        eprintln!("fatal bot error: {e}");
        std::process::exit(1);
    }
}
