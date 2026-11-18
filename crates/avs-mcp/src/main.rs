use avs_mcp::{Server, contract, serve};
use avs_session::Env;
use std::io;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [] => {}
        [flag, root] if flag == "--write-contract" => match contract::write(std::path::Path::new(root)) {
            Ok(count) => {
                println!("wrote {count} contract files under {root}");
                return;
            }
            Err(error) => {
                eprintln!("agent-video-studio-mcp: {error}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("usage: agent-video-studio-mcp [--write-contract <repository root>]");
            std::process::exit(2);
        }
    }
    let server = Server::new(Env::from_process());
    if let Err(error) = serve(&server, io::stdin().lock(), io::stdout().lock()) {
        eprintln!("agent-video-studio-mcp: {error}");
        std::process::exit(1);
    }
}
