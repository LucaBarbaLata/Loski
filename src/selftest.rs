//! `loski selftest`: checks the Spotify features against your real account
//! without Discord. Prints counts and a few names, nothing else.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::{
    config::Config,
    spotify::{
        self, Spotify,
        web::{SearchKind, WebApi},
    },
};

pub async fn run(cfg: Arc<Config>) -> Result<()> {
    let http = reqwest::Client::new();
    let credentials = spotify::cached_credentials(&cfg)
        .context("no stored Spotify login; start Loski once first")?;
    let token = WebApi::load_token(&cfg).context("no stored Web API token")?;
    let spotify = Spotify::connect(cfg.clone(), http, credentials, token).await?;
    println!("✓ connected");

    for kind in SearchKind::ALL {
        let items = spotify.web.search("midnight city", kind, 5).await?;
        let first = items
            .first()
            .map(|i| format!("{} ({})", i.title, i.subtitle));
        println!(
            "✓ search {}: {} results, first: {}",
            kind.label(),
            items.len(),
            first.unwrap_or_default()
        );
    }

    let seed = spotify
        .web
        .search_tracks("midnight city m83", 1)
        .await?
        .into_iter()
        .next()
        .context("no seed song found")?;
    let radio = spotify.radio(&seed).await?;
    let sample: Vec<_> = radio.tracks.iter().take(3).map(|t| t.display()).collect();
    println!(
        "✓ radio for {}: {} songs, e.g. {}",
        seed.title,
        radio.tracks.len(),
        sample.join(" | ")
    );

    match spotify.liked_songs(true).await {
        Ok(liked) => println!(
            "✓ liked songs: loaded {} (more available: {})",
            liked.tracks.len(),
            liked.truncated
        ),
        Err(e) => println!("✗ liked songs: {e:#}"),
    }

    let devices = spotify.web.device_names().await?;
    let loski = devices.iter().any(|d| *d == cfg.bot_name);
    println!(
        "{} Spotify Connect devices online: {} ({} listed)",
        if loski { "✓" } else { "✗" },
        devices.join(", "),
        cfg.bot_name
    );

    let playlists = spotify.my_playlists().await?;
    let names: Vec<_> = playlists.iter().take(3).map(|p| p.name.as_str()).collect();
    println!(
        "✓ your playlists: {}, first: {}",
        playlists.len(),
        names.join(", ")
    );
    Ok(())
}
