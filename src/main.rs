mod game;
mod plugin;
mod protocol;
mod scoring;
mod server;
mod store;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use plugin::BonesGamePlugin;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use store::StoreEvent;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bones=info,tower_http=info".into()),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let handle = rt.handle().clone();

    let store = match std::env::var("REDIS_URL") {
        Ok(url) if !url.is_empty() => Some(rt.block_on(store::connect_with_retry(&url, handle))),
        _ => {
            tracing::warn!("REDIS_URL unset; rooms stay in memory on this process");
            None
        }
    };

    let channels = server::new_channels(store.clone());
    if let Some(store) = store.clone() {
        let sub_channels = channels.clone();
        let shutdown = channels.shutdown.subscribe();
        let subscriber = store.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        rt.spawn(async move {
            if let Err(err) = subscriber
                .subscribe(sub_channels, shutdown, Some(ready_tx))
                .await
            {
                tracing::error!("redis subscriber ended: {err}");
            }
        });
        let _ = rt.block_on(ready_rx);
        match store.scan_all() {
            Ok(rooms) => {
                if let Ok(mut q) = channels.remote_events.lock() {
                    q.extend(rooms.into_iter().map(|room| StoreEvent::Upsert { room }));
                }
            }
            Err(err) => tracing::warn!("redis hydrate failed: {err}"),
        }
    }

    let web_dir = web_dir();
    let addr: SocketAddr = std::env::var("BONES_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()
        .expect("BONES_ADDR");

    let shutdown = channels.shutdown.clone();
    let bevy_channels = channels.clone();
    let bevy = std::thread::spawn(move || {
        App::new()
            .add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
                Duration::from_millis(50),
            )))
            .insert_resource(bevy_channels)
            .add_plugins(BonesGamePlugin)
            .run();
    });

    rt.block_on(server::serve(channels, web_dir, addr));

    let _ = shutdown.send(true);
    let _ = bevy.join();
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
