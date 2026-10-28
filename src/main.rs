mod audio;
mod banner;
mod color;
mod config;
mod discord;
mod dsp;
mod engine;
mod genres;
mod logging;
mod personality;
mod picture;
mod selftest;
mod setup;
mod speech;
mod spotify;
mod stats;
mod store;
mod track;
#[cfg(unix)]
mod user;
mod util;

use std::sync::Arc;

fn main() {
    let _ = dotenvy::dotenv();
    // Before any thread starts: in Docker, from root to Loski's own user.
    #[cfg(unix)]
    user::become_loski(&setup::data_dir());
    let logs = logging::init();
    let code = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the async runtime starts")
        .block_on(run());
    // Write out whatever the log file still has buffered.
    drop(logs);
    if code != 0 {
        std::process::exit(code);
    }
}

async fn run() -> i32 {
    let data_dir = setup::data_dir();
    setup::load(&data_dir);
    let selftest = std::env::args().nth(1).as_deref() == Some("selftest");

    // Without an owner yet, whoever DMs Loski this code becomes it.
    let owner_known = std::env::var("OWNER_ID").is_ok_and(|v| !v.trim().is_empty())
        || data_dir.join(discord::OWNER_MARKER).exists();
    let claim = (!owner_known && !selftest).then(discord::claim_code);

    if !selftest && !setup::missing().is_empty() {
        let port = std::env::var("AUTH_PORT")
            .ok()
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(5588);
        tracing::warn!(
            "Setup needed: open http://127.0.0.1:{port}/ in your browser to give Loski its Discord token and Spotify keys."
        );
        if let Err(e) = setup::run(&data_dir, port, claim.as_deref()).await {
            tracing::error!("Loski can't start: {e:#}");
            return 2;
        }
        setup::load(&data_dir);
    }

    let cfg = match config::Config::from_env() {
        Ok(cfg) => Arc::new(cfg),
        Err(e) => {
            tracing::error!("Loski can't start: {e:#}");
            return 2;
        }
    };

    if selftest {
        if let Err(e) = selftest::run(cfg).await {
            tracing::error!("selftest failed: {e:#}");
            return 1;
        }
        return 0;
    }

    if let Err(e) = discord::run(cfg, claim).await {
        tracing::error!("{e:#}");
        return 1;
    }
    0
}
