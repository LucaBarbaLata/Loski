//! Keeps Loski available as a Spotify Connect speaker. Pick it in the
//! Spotify app's device list and the app becomes the remote while the music
//! plays in Discord.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{Session, SessionConfig, config::DeviceType};
use librespot_playback::{
    mixer::{Mixer as _, MixerConfig, softmixer::SoftMixer},
    player::Player,
};
use rand::Rng;

use super::{cache, cached_credentials};
use crate::{
    audio::{CONNECT_DECK, Mixer, PipeSink},
    config::Config,
    engine::{Engine, player_config},
};

/// The same device id across restarts, so the Spotify app doesn't collect
/// stale "Loski" entries.
fn device_id(cfg: &Config) -> String {
    let path = cfg.data_dir.join("connect_device_id");
    if let Ok(id) = std::fs::read_to_string(&path)
        && id.trim().len() == 40
    {
        return id.trim().to_string();
    }
    let id: String = (0..20)
        .map(|_| format!("{:02x}", rand::rng().random::<u8>()))
        .collect();
    let _ = std::fs::write(&path, &id);
    id
}

async fn start(
    cfg: &Config,
    engine: &Arc<Engine>,
    mixer: Arc<Mixer>,
) -> Result<impl Future<Output = ()> + use<>> {
    let credentials = cached_credentials(cfg).context("no stored Spotify login")?;
    let session = Session::new(
        SessionConfig {
            device_id: device_id(cfg),
            ..SessionConfig::default()
        },
        Some(cache(cfg)?),
    );
    // Volume set from the Spotify app is applied here, on top of Loski's own.
    let volume = Arc::new(SoftMixer::open(MixerConfig::default()).map_err(|e| anyhow!("{e}"))?);
    let player = Player::new(
        player_config(engine.normalise()),
        session.clone(),
        volume.get_soft_volume(),
        move || Box::new(PipeSink::new(mixer, CONNECT_DECK)),
    );
    let events = player.get_player_event_channel();
    let config = ConnectConfig {
        name: cfg.bot_name.clone(),
        device_type: DeviceType::Speaker,
        initial_volume: u16::MAX,
        ..ConnectConfig::default()
    };
    let (spirc, task) = Spirc::new(config, session, credentials, player.clone(), volume)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    engine.attach_connect(player, Arc::new(spirc), events);
    Ok(task)
}

/// Runs the Connect device for as long as Loski runs, restarting it if
/// its connection drops.
pub async fn run(cfg: Arc<Config>, engine: Arc<Engine>) {
    loop {
        match start(&cfg, &engine, engine.mixer()).await {
            Ok(task) => {
                tracing::info!("Loski is available as a Spotify Connect device");
                task.await;
                tracing::warn!("Spotify Connect stopped; restarting it");
            }
            Err(e) => tracing::warn!("Couldn't start Spotify Connect: {e:#}"),
        }
        engine.detach_connect();
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}
