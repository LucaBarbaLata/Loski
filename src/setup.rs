//! First-run setup. Without a Discord token or Spotify keys yet, Loski
//! serves a page at http://127.0.0.1:5588 that asks for them, checks them
//! with Discord and Spotify, and saves them in `data/setup.env`, so nobody
//! has to edit `.env` by hand. Anything set in `.env` still wins.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Where the setup page keeps what you typed, inside the data folder.
pub const FILE: &str = "setup.env";
/// What Loski can't run without, and the setup page asks for.
const NEEDED: [&str; 3] = [
    "DISCORD_TOKEN",
    "SPOTIFY_CLIENT_ID",
    "SPOTIFY_CLIENT_SECRET",
];
/// The permissions in the invite link (see the README).
const PERMISSIONS: &str = "281474979941376";

fn set(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| !v.trim().is_empty())
}

/// The keys still missing.
pub fn missing() -> Vec<&'static str> {
    NEEDED.into_iter().filter(|key| !set(key)).collect()
}

pub fn data_dir() -> PathBuf {
    std::env::var("DATA_DIR")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

/// Reads `data/setup.env` into the environment, for whatever `.env` left
/// unset or empty.
pub fn load(dir: &Path) {
    let Ok(entries) = dotenvy::from_path_iter(dir.join(FILE)) else {
        return;
    };
    for (key, value) in entries.flatten() {
        if !set(&key) {
            // SAFETY: still at startup, before any other thread reads the environment.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

/// What the checks found out about the Discord bot.
struct Bot {
    name: String,
    app_id: String,
}

#[derive(Deserialize)]
struct Application {
    id: String,
    name: String,
}

async fn check_discord(http: &reqwest::Client, token: &str) -> Result<Bot> {
    let response = http
        .get("https://discord.com/api/v10/applications/@me")
        .header("Authorization", format!("Bot {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("couldn't reach Discord")?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!(
            "Discord didn't accept that bot token. Copy it again from Bot → Reset Token (the whole thing)."
        );
    }
    let app: Application = response
        .error_for_status()
        .context("Discord said no")?
        .json()
        .await?;
    Ok(Bot {
        name: app.name,
        app_id: app.id,
    })
}

async fn check_spotify(http: &reqwest::Client, id: &str, secret: &str) -> Result<()> {
    let response = http
        .post("https://accounts.spotify.com/api/token")
        .basic_auth(id, Some(secret))
        .form(&[("grant_type", "client_credentials")])
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("couldn't reach Spotify")?;
    if !response.status().is_success() {
        bail!(
            "Spotify didn't accept that Client ID and secret. Copy both again from your app's settings."
        );
    }
    Ok(())
}

fn save(dir: &Path, values: &[(&str, &str)]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut text = String::from(
        "# Written by Loski's setup page. Anything also set in .env is taken from there instead.\n",
    );
    for (key, value) in values {
        text.push_str(&format!("{key}={value}\n"));
    }
    let path = dir.join(FILE);
    crate::store::write_atomic(&path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Tokens: readable by Loski only.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    body: String,
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut first = head.lines().next()?.split_whitespace();
    let method = first.next()?.to_string();
    let path = first.next()?.split('?').next()?.to_string();
    let length: usize = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
        .min(64 * 1024);
    while buf.len() < head_end + length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = buf.len().min(head_end + length);
    let body = String::from_utf8_lossy(&buf[head_end..end]).to_string();
    Some(Request { method, path, body })
}

async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = r#"
body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0f1012;color:#e8e8ea;font:16px/1.55 system-ui,-apple-system,Segoe UI,Roboto,Noto Sans,sans-serif;padding:24px 16px;box-sizing:border-box}
main{max-width:36rem;width:100%}
h1{font-size:1.6rem;margin:.25rem 0 .5rem}h2{font-size:1rem;margin:1.6rem 0 .4rem}
p,li{color:#b4b5ba}a{color:#f2a33a}code{background:#1c1d21;padding:.1em .35em;border-radius:4px;color:#e8e8ea}
ol{padding-left:1.2rem}li{margin:.25rem 0}
label{display:block;margin:1rem 0 .3rem;font-weight:600;color:#e8e8ea}
input{width:100%;box-sizing:border-box;padding:.7rem .8rem;border-radius:10px;border:1px solid #33353b;background:#16171a;color:#e8e8ea;font:inherit}
input:focus{outline:2px solid #f2a33a;outline-offset:1px}
button,.btn{display:inline-block;margin-top:1.4rem;padding:.7rem 1.4rem;border-radius:999px;border:0;background:#f2a33a;color:#111;font:inherit;font-weight:700;text-decoration:none;cursor:pointer}
.error{background:#3a1616;border:1px solid #7a2a2a;color:#ffb4b4;padding:.8rem 1rem;border-radius:10px}
.step{background:#16171a;border:1px solid #26282d;border-radius:14px;padding:1rem 1.2rem;margin:1rem 0}
.step>h2:first-child{margin-top:0}.step>:last-child{margin-bottom:0}.step .btn{margin-top:.25rem}
h1,h2{font-weight:700}
.claim{font-size:1.4rem;letter-spacing:.06em}
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{} · Loski</title><style>{STYLE}</style></head><body><main><div style="font-size:2.4rem">🎧</div>{body}</main></body></html>"#,
        escape(title)
    )
}

fn form(port: u16, error: Option<&str>, values: &HashMap<String, String>) -> String {
    let value = |key: &str| escape(values.get(key).map_or("", String::as_str));
    let error = error
        .map(|e| format!(r#"<p class="error">{}</p>"#, escape(e)))
        .unwrap_or_default();
    page(
        "Set up",
        &format!(
            r#"<h1>Set up Loski</h1>
<p>Two sets of keys, then you're done. They're saved on this computer, in <code>data/{FILE}</code>.</p>
{error}
<div class="step"><h2>1. A Discord bot</h2><ol>
<li>Open <a href="https://discord.com/developers/applications" target="_blank" rel="noreferrer">discord.com/developers/applications</a> and click <b>New Application</b>. Name it <b>Loski</b>.</li>
<li>Open <b>Bot</b>, click <b>Reset Token</b> and copy it.</li></ol></div>
<div class="step"><h2>2. A Spotify app</h2><ol>
<li>Open <a href="https://developer.spotify.com/dashboard" target="_blank" rel="noreferrer">developer.spotify.com/dashboard</a> and click <b>Create app</b>.</li>
<li>Set the redirect URI to exactly <code>http://127.0.0.1:{port}/callback</code> and tick <b>Web API</b>.</li>
<li>Copy the <b>Client ID</b> and <b>Client Secret</b>.</li></ol></div>
<form method="post" action="/setup" autocomplete="off">
<label for="token">Discord bot token</label><input id="token" name="DISCORD_TOKEN" required value="{}" spellcheck="false">
<label for="id">Spotify Client ID</label><input id="id" name="SPOTIFY_CLIENT_ID" required value="{}" spellcheck="false">
<label for="secret">Spotify Client Secret</label><input id="secret" name="SPOTIFY_CLIENT_SECRET" type="password" required value="{}" spellcheck="false">
<button type="submit">Check and save</button></form>"#,
            value("DISCORD_TOKEN"),
            value("SPOTIFY_CLIENT_ID"),
            value("SPOTIFY_CLIENT_SECRET"),
        ),
    )
}

fn done(port: u16, bot: &Bot, claim: Option<&str>) -> String {
    let invite = format!(
        "https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions={PERMISSIONS}",
        bot.app_id
    );
    let claim = claim
        .map(|code| {
            format!(
                r#"<div class="step"><h2>2. Make it yours</h2><p>In your server, find <b>{}</b> in the member list, open a direct message and send:</p><p class="claim"><code>claim {code}</code></p><p>Whoever sends it first becomes the owner. The code is only on this page and in Loski's log.</p></div>"#,
                escape(&bot.name)
            )
        })
        .unwrap_or_default();
    let spotify_step = if claim.is_empty() { 2 } else { 3 };
    page(
        "Saved",
        &format!(
            r#"<h1>{} is set up ✓</h1>
<div class="step"><h2>1. Invite it to your server</h2><p><a class="btn" href="{invite}" target="_blank" rel="noreferrer">Invite {}</a></p></div>
{claim}
<div class="step"><h2>{spotify_step}. Log in to Spotify</h2><p>Loski is starting now. In a few seconds this button opens the Spotify login (two quick approvals):</p><p><a class="btn" href="http://127.0.0.1:{port}/">Log in to Spotify</a></p></div>"#,
            escape(&bot.name),
            escape(&bot.name)
        ),
    )
}

/// Serves the setup page until the keys are checked and saved. `claim` is
/// the code that makes someone the owner, when there isn't one yet.
pub async fn run(dir: &Path, port: u16, claim: Option<&str>) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("could not open the setup page on port {port}"))?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("Loski/", env!("CARGO_PKG_VERSION")))
        .build()?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let Some(request) = read_request(&mut stream).await else {
            continue;
        };
        if request.method != "POST" || request.path != "/setup" {
            let html = form(port, None, &HashMap::new());
            respond(&mut stream, "200 OK", &html).await;
            continue;
        }
        let values: HashMap<String, String> = url::form_urlencoded::parse(request.body.as_bytes())
            .map(|(k, v)| (k.to_string(), v.trim().to_string()))
            .collect();
        let get = |key: &str| values.get(key).map_or("", String::as_str);
        let (token, id, secret) = (
            get("DISCORD_TOKEN"),
            get("SPOTIFY_CLIENT_ID"),
            get("SPOTIFY_CLIENT_SECRET"),
        );
        let checked = async {
            if token.is_empty() || id.is_empty() || secret.is_empty() {
                bail!("All three are needed.");
            }
            let bot = check_discord(&http, token).await?;
            check_spotify(&http, id, secret).await?;
            save(
                dir,
                &[
                    ("DISCORD_TOKEN", token),
                    ("SPOTIFY_CLIENT_ID", id),
                    ("SPOTIFY_CLIENT_SECRET", secret),
                ],
            )
            .context("couldn't save the keys")?;
            Ok(bot)
        }
        .await;
        match checked {
            Ok(bot) => {
                respond(&mut stream, "200 OK", &done(port, &bot, claim)).await;
                tracing::info!("Setup saved for the bot {}", bot.name);
                return Ok(());
            }
            Err(e) => {
                let html = form(port, Some(&format!("{e:#}")), &values);
                respond(&mut stream, "400 Bad Request", &html).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_escape_what_you_typed() {
        let values: HashMap<String, String> =
            [("DISCORD_TOKEN".to_string(), "\"><b>x".to_string())]
                .into_iter()
                .collect();
        let html = form(5588, Some("<oops>"), &values);
        assert!(html.contains("&quot;&gt;&lt;b&gt;x"));
        assert!(html.contains("&lt;oops&gt;"));
        assert!(html.contains("http://127.0.0.1:5588/callback"));
        let bot = Bot {
            name: "Loski".into(),
            app_id: "123".into(),
        };
        let html = done(5588, &bot, Some("482913"));
        assert!(html.contains("claim 482913") && html.contains("client_id=123"));
        // The CSS mentions the claim style either way; the step itself is gone.
        assert!(!done(5588, &bot, None).contains("Make it yours"));
    }

    #[test]
    fn saves_and_loads_the_keys() {
        let dir = std::env::temp_dir().join(format!("loski-setup-{}", std::process::id()));
        save(&dir, &[("LOSKI_TEST_SETUP_KEY", "abc.def-123")]).unwrap();
        load(&dir);
        assert_eq!(
            std::env::var("LOSKI_TEST_SETUP_KEY").unwrap(),
            "abc.def-123"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
