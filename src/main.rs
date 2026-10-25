mod audio;
mod banner;
mod color;
mod config;
mod discord;
mod dsp;
mod engine;
mod logging;
mod personality;
mod picture;
mod selftest;
mod spotify;
mod stats;
mod store;
mod track;
mod util;

use std::sync::Arc;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let logs = logging::init();
    let code = run().await;
    // Write out whatever the log file still has buffered.
    drop(logs);
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run() -> i32 {
    let cfg = match config::Config::from_env() {
        Ok(cfg) => Arc::new(cfg),
        Err(e) => {
            tracing::error!("Loski can't start: {e:#}");
            return 2;
        }
    };

    if std::env::args().nth(1).as_deref() == Some("selftest") {
        if let Err(e) = selftest::run(cfg).await {
            tracing::error!("selftest failed: {e:#}");
            return 1;
        }
        return 0;
    }

    if let Err(e) = discord::run(cfg).await {
        tracing::error!("{e:#}");
        return 1;
    }
    0
}
