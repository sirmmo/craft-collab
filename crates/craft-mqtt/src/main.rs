//! `craft-mqtt`: live co-editing over MQTT for the craft apps. `craft-mqtt --manifest` prints its
//! `plugin.json`; otherwise it answers the app on standard input and output.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

fn main() {
    if std::env::args().any(|a| a == "--manifest") {
        println!("{:#}", craft_mqtt::manifest::manifest());
        return;
    }
    craft_collab_kit::runtime::serve(craft_mqtt::commands::Mqtt::new(), std::io::stdin(), std::io::stdout());
}
