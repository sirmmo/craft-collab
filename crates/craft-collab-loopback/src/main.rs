//! `craft-collab-loopback`: co-editing between app processes on one computer. `--manifest`
//! prints its `plugin.json`; `--relay` runs only the relay (in the foreground); otherwise it
//! answers the app on standard input and output.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--manifest") {
        println!("{:#}", craft_collab_loopback::manifest::manifest());
        return;
    }
    if args.iter().any(|a| a == "--relay") {
        let port = craft_collab_loopback::port();
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => {
                eprintln!("craft-collab-loopback: relay on 127.0.0.1:{port}");
                craft_collab_loopback::relay::serve(l);
            }
            Err(e) => eprintln!("craft-collab-loopback: can't listen on 127.0.0.1:{port}: {e}"),
        }
        return;
    }
    craft_collab_kit::runtime::serve(craft_collab_loopback::commands::Loopback::new(), std::io::stdin(), std::io::stdout());
}
