mod game;
mod plugin;
mod protocol;
mod scoring;
mod server;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use plugin::BonesGamePlugin;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bones=info,tower_http=info".into()),
        )
        .init();

    let channels = server::new_channels();
    let web_dir = web_dir();
    let addr: SocketAddr = std::env::var("BONES_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()
        .expect("BONES_ADDR");

    let net = channels.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(server::serve(net, web_dir, addr));
    });

    std::thread::sleep(Duration::from_millis(50));

    App::new()
        .add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_millis(
            50,
        ))))
        .insert_resource(channels)
        .add_plugins(BonesGamePlugin)
        .run();
}

fn web_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BONES_WEB_DIR") {
        return PathBuf::from(dir);
    }
    let candidates = [
        PathBuf::from("web"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web"),
    ];
    for path in candidates {
        if path.join("index.html").exists() {
            return path;
        }
    }
    PathBuf::from("web")
}
