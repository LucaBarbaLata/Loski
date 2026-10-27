//! One-time browser login.
//!
//! Two logins happen back to back in the same browser tab:
//! 1. librespot, with Spotify's own client ID (needed to stream audio).
//! 2. Your developer app (used for search and follow mode).
//!
//! Spotify sends the browser back to 127.0.0.1, so the callback server here
//! only hears it when the browser runs on the same computer as Loski. From
//! anywhere else (Loski on a server, say), the page Spotify sends you back to
//! doesn't load, and you paste its address to Loski in Discord instead: it
//! carries the same code, which ends up in `Login::finish` either way.
//!
//! The callback server listens on all interfaces because inside Docker the
//! browser's request arrives through a port mapping, not on loopback.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{Rng, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
};
use url::Url;

use crate::config::{Config, LIBRESPOT_AUTH_PORT};

const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const LIBRESPOT_SCOPES: &str = "streaming";
const WEB_SCOPES: &str = "user-read-playback-state user-read-currently-playing";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

fn librespot_redirect_uri() -> String {
    format!("http://127.0.0.1:{LIBRESPOT_AUTH_PORT}/login")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp in seconds.
    pub expires_at: i64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

pub struct LoginResult {
    /// Short-lived token used once to create stored librespot credentials.
    pub librespot_token: Option<String>,
    pub web_token: Option<WebToken>,
}

/// The address Spotify sent the browser back to, pasted in Discord because
/// the browser wasn't on Loski's computer, and where to say how it went.
pub struct Pasted {
    pub text: String,
    /// What to tell you: how far the login got, or what was wrong.
    pub reply: oneshot::Sender<Result<String, String>>,
}

/// The code and its check from a pasted address, and which login it ends:
/// `/login` for streaming, `/callback` for search.
fn read_pasted(text: &str) -> Option<(String, HashMap<String, String>)> {
    let word = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c| matches!(c, '<' | '>' | '`' | '"' | '\'')))
        .find(|w| w.contains("code=") || w.contains("error="))?;
    let address = if word.contains("://") {
        word.to_string()
    } else if word.starts_with('/') {
        // Just the end of it, like "/login?code=…".
        format!("http://127.0.0.1{word}")
    } else {
        // Without the "http://" a browser hides, like "127.0.0.1:5588/callback?code=…".
        format!("http://{word}")
    };
    let url = Url::parse(&address).ok()?;
    Some((
        url.path().to_string(),
        url.query_pairs().into_owned().collect(),
    ))
}

/// One Spotify login: the approval pages to open, and what it takes to
/// finish them.
pub struct Login {
    verifier: String,
    state: String,
    librespot_url: Option<Url>,
    web_url: Option<Url>,
}

impl Login {
    /// Prepares the logins still needed (streaming, search or both).
    pub fn new(cfg: &Config, need_librespot: bool, need_web: bool) -> Result<Self> {
        let verifier: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(96)
            .map(char::from)
            .collect();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(24)
            .map(char::from)
            .collect();
        let librespot_url = Url::parse_with_params(
            "https://accounts.spotify.com/authorize",
            &[
                ("response_type", "code"),
                ("client_id", KEYMASTER_CLIENT_ID),
                ("redirect_uri", &librespot_redirect_uri()),
                ("scope", LIBRESPOT_SCOPES),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
                ("state", &state),
            ],
        )?;
        let web_url = Url::parse_with_params(
            "https://accounts.spotify.com/authorize",
            &[
                ("response_type", "code"),
                ("client_id", cfg.spotify_client_id.as_str()),
                ("redirect_uri", &cfg.web_redirect_uri()),
                ("scope", WEB_SCOPES),
                ("state", &state),
            ],
        )?;
        Ok(Self {
            verifier,
            state,
            librespot_url: need_librespot.then_some(librespot_url),
            web_url: need_web.then_some(web_url),
        })
    }

    /// The approval pages, streaming first, for a browser anywhere.
    pub fn links(&self) -> (Option<String>, Option<String>) {
        (
            self.librespot_url.as_ref().map(Url::to_string),
            self.web_url.as_ref().map(Url::to_string),
        )
    }

    /// The streaming login's code, swapped for its token.
    async fn streaming(&self, http: &reqwest::Client, code: &str) -> Result<String> {
        exchange_librespot_code(http, code, &self.verifier).await
    }

    /// What a pasted address does: finishes one of the logins, or says why not.
    async fn paste(
        &self,
        cfg: &Config,
        http: &reqwest::Client,
        text: &str,
        result: &mut LoginResult,
    ) -> Result<String, String> {
        let Some((path, query)) = read_pasted(text) else {
            return Err("That doesn't look like the address from the login. It starts with `http://127.0.0.1` and has `code=` in it.".into());
        };
        if query.get("state") != Some(&self.state) {
            return Err(
                "That address is from an older login. Use the buttons in my latest message.".into(),
            );
        }
        if let Some(error) = query.get("error") {
            return Err(format!("Spotify said: {error}. Press the button again."));
        }
        let code = query.get("code").map(String::as_str).unwrap_or_default();
        match path.as_str() {
            "/login" if self.librespot_url.is_some() => {
                let token = self.streaming(http, code).await.map_err(|e| {
                    tracing::error!("librespot login failed: {e:#}");
                    format!("Spotify didn't take it: {e}. Press the button again.")
                })?;
                result.librespot_token = Some(token);
            }
            "/callback" if self.web_url.is_some() => {
                let token = exchange_web_code(cfg, http, code).await.map_err(|e| {
                    tracing::error!("Web API login failed: {e:#}");
                    format!("Spotify didn't take it: {e}. Press the button again.")
                })?;
                result.web_token = Some(token);
            }
            _ => {
                return Err(
                    "That's an address I wasn't waiting for. Use the buttons in my latest message."
                        .into(),
                );
            }
        }
        Ok(if self.done(result) {
            "**Spotify is connected.** Give me a moment to start, then `/play` away.".into()
        } else if result.librespot_token.is_some() {
            "Streaming is set up (1 of 2). Now the second button: search and follow.".into()
        } else {
            "Search is set up (1 of 2). Now the first button: streaming.".into()
        })
    }

    fn done(&self, r: &LoginResult) -> bool {
        (self.librespot_url.is_none() || r.librespot_token.is_some())
            && (self.web_url.is_none() || r.web_token.is_some())
    }

    /// Waits until every login has finished: through the login page on this
    /// computer, or addresses pasted in Discord.
    pub async fn finish(
        &self,
        cfg: &Config,
        http: &reqwest::Client,
        pasted: &mut mpsc::UnboundedReceiver<Pasted>,
    ) -> Result<LoginResult> {
        let web_listener = TcpListener::bind(("0.0.0.0", cfg.auth_port))
            .await
            .with_context(|| format!("could not open login port {}", cfg.auth_port))?;
        let librespot_listener = TcpListener::bind(("0.0.0.0", LIBRESPOT_AUTH_PORT))
            .await
            .with_context(|| format!("could not open login port {LIBRESPOT_AUTH_PORT}"))?;
        let next_urls = NextUrls {
            librespot: self.librespot_url.clone(),
            web: self.web_url.clone(),
        };
        let mut result = LoginResult {
            librespot_token: None,
            web_token: None,
        };
        let mut pasting = true;

        loop {
            let stream = tokio::select! {
                accepted = web_listener.accept() => accepted?.0,
                accepted = librespot_listener.accept() => accepted?.0,
                paste = pasted.recv(), if pasting => {
                    match paste {
                        Some(paste) => {
                            let reply = self.paste(cfg, http, &paste.text, &mut result).await;
                            let _ = paste.reply.send(reply);
                            if self.done(&result) {
                                return Ok(result);
                            }
                        }
                        None => pasting = false,
                    }
                    continue;
                }
            };
            let Some((mut stream, path, query)) = read_request(stream).await else {
                continue;
            };

            if query.get("state").is_some_and(|s| s != &self.state) {
                respond_html(
                    &mut stream,
                    400,
                    &page(
                        "That login link is stale",
                        "Open the login page again and start over.",
                        Some(&cfg.login_url()),
                    ),
                )
                .await;
                continue;
            }
            if let Some(error) = query.get("error") {
                respond_html(
                    &mut stream,
                    400,
                    &page(
                        "Login was cancelled",
                        &format!("Spotify said: {error}. You can try again."),
                        Some(&cfg.login_url()),
                    ),
                )
                .await;
                continue;
            }

            match path.as_str() {
                "/login" => match query.get("code") {
                    Some(code) => match self.streaming(http, code).await {
                        Ok(token) => {
                            result.librespot_token = Some(token);
                            continue_login(&mut stream, &result, &next_urls).await;
                        }
                        Err(e) => {
                            tracing::error!("librespot login failed: {e:#}");
                            respond_html(
                                &mut stream,
                                500,
                                &page("Login failed", &format!("{e}"), Some(&cfg.login_url())),
                            )
                            .await;
                        }
                    },
                    None => redirect(&mut stream, &cfg.login_url()).await,
                },
                "/callback" => match query.get("code") {
                    Some(code) => match exchange_web_code(cfg, http, code).await {
                        Ok(token) => {
                            result.web_token = Some(token);
                            continue_login(&mut stream, &result, &next_urls).await;
                        }
                        Err(e) => {
                            tracing::error!("Web API login failed: {e:#}");
                            respond_html(&mut stream, 500, &page("Login failed", &format!("{e}. Check SPOTIFY_CLIENT_ID, SPOTIFY_CLIENT_SECRET and that the redirect URI in your Spotify app is exactly {}", cfg.web_redirect_uri()), Some(&cfg.login_url()))).await;
                        }
                    },
                    None => redirect(&mut stream, &cfg.login_url()).await,
                },
                "/" => continue_login(&mut stream, &result, &next_urls).await,
                _ => respond_html(&mut stream, 404, "").await,
            }

            if self.done(&result) {
                return Ok(result);
            }
        }
    }
}

/// Where the browser goes for each login that's still missing.
struct NextUrls {
    librespot: Option<Url>,
    web: Option<Url>,
}

/// Sends the browser to the next login it still needs, or shows the success
/// page. Done in the same response, because once the last login finishes
/// this server stops and a redirect back to it would land on nothing.
async fn continue_login(stream: &mut TcpStream, result: &LoginResult, next: &NextUrls) {
    match (&next.librespot, &next.web) {
        (Some(url), _) if result.librespot_token.is_none() => redirect(stream, url.as_str()).await,
        (_, Some(url)) if result.web_token.is_none() => redirect(stream, url.as_str()).await,
        _ => {
            respond_html(
                stream,
                200,
                &page(
                    "Loski is connected ✓",
                    "You can close this tab and go back to Discord.",
                    None,
                ),
            )
            .await
        }
    }
}

pub async fn refresh_web_token(
    cfg: &Config,
    http: &reqwest::Client,
    refresh_token: &str,
) -> Result<WebToken> {
    let resp = http
        .post(TOKEN_URL)
        .basic_auth(&cfg.spotify_client_id, Some(&cfg.spotify_client_secret))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?;
    let token = parse_token(resp).await?;
    Ok(WebToken {
        access_token: token.access_token,
        // Spotify may or may not rotate the refresh token.
        refresh_token: token
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at: now_unix() + token.expires_in,
    })
}

async fn exchange_librespot_code(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<String> {
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &librespot_redirect_uri()),
            ("client_id", KEYMASTER_CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    Ok(parse_token(resp).await?.access_token)
}

async fn exchange_web_code(cfg: &Config, http: &reqwest::Client, code: &str) -> Result<WebToken> {
    let resp = http
        .post(TOKEN_URL)
        .basic_auth(&cfg.spotify_client_id, Some(&cfg.spotify_client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &cfg.web_redirect_uri()),
        ])
        .send()
        .await?;
    let token = parse_token(resp).await?;
    Ok(WebToken {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .ok_or_else(|| anyhow!("Spotify did not return a refresh token"))?,
        expires_at: now_unix() + token.expires_in,
    })
}

async fn parse_token(resp: reqwest::Response) -> Result<TokenResponse> {
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        bail!("Spotify returned {status}: {body}");
    }
    serde_json::from_str(&body).context("unexpected token response from Spotify")
}

async fn read_request(
    mut stream: TcpStream,
) -> Option<(TcpStream, String, HashMap<String, String>)> {
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    loop {
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read(&mut buf[len..]),
        )
        .await
        .ok()?
        .ok()?;
        if n == 0 {
            break;
        }
        len += n;
        if buf[..len].windows(4).any(|w| w == b"\r\n\r\n") || len == buf.len() {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..len]);
    let target = head.lines().next()?.split_whitespace().nth(1)?.to_string();
    let url = Url::parse(&format!("http://localhost{target}")).ok()?;
    let query = url.query_pairs().into_owned().collect();
    Some((stream, url.path().to_string(), query))
}

async fn redirect(stream: &mut TcpStream, location: &str) {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn respond_html(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn page(title: &str, message: &str, retry: Option<&str>) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let retry = retry
        .map(|url| format!(r#"<a class="btn" href="{url}">Try again</a>"#))
        .unwrap_or_default();
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Loski</title><style>
body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#0f1012;color:#e8e8ea;font:16px/1.5 system-ui,-apple-system,Segoe UI,sans-serif}}
main{{max-width:28rem;padding:2rem;text-align:center}}
.logo{{font-size:2.75rem}}h1{{font-size:1.4rem;margin:.5rem 0}}p{{color:#a7a8ad}}
.btn{{display:inline-block;margin-top:1rem;padding:.6rem 1.2rem;border-radius:999px;background:#1ed760;color:#0b0b0b;text-decoration:none;font-weight:600}}
</style></head><body><main><div class="logo">🎧</div><h1>{}</h1><p>{}</p>{retry}</main></body></html>"#,
        escape(title),
        escape(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_address_you_paste() {
        let (path, query) =
            read_pasted("here: <http://127.0.0.1:8898/login?code=AQB-x_9&state=Zx12> thanks")
                .unwrap();
        assert_eq!(path, "/login");
        assert_eq!(query["code"], "AQB-x_9");
        assert_eq!(query["state"], "Zx12");
        let (path, query) = read_pasted("127.0.0.1:5588/callback?code=abc&state=s").unwrap();
        assert_eq!(
            (path.as_str(), query["code"].as_str()),
            ("/callback", "abc")
        );
        let (_, query) =
            read_pasted("http://127.0.0.1:5588/callback?error=access_denied&state=s").unwrap();
        assert_eq!(query["error"], "access_denied");
        let (path, _) = read_pasted("/login?code=abc&state=s").unwrap();
        assert_eq!(path, "/login");
        assert!(read_pasted("hello there").is_none());
    }
}
