//! Signing in to a subscription (ChatGPT, Grok) in the browser, as the
//! TypeScript CLI does (`src/oauth/`): PKCE with a local callback, the
//! code exchanged for tokens, the tokens saved in `auth.json`.
//!
//! [`begin`] binds the callback and builds the address to open; [`Login::finish`]
//! waits for the browser to come back (five minutes at most) and saves the
//! sign-in. [`import_grok_cli`] takes an existing Grok CLI sign-in instead.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::auth::{AuthFile, Subscription, account_id_in, now_ms};

/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(5 * 60);

const CHATGPT_AUTHORIZE: &str = "https://auth.openai.com/oauth/authorize";
const CHATGPT_SCOPE: &str = "openid profile email offline_access";
/// The one redirect the ChatGPT client allows.
const CHATGPT_PORT: u16 = 1455;
const GROK_AUTHORIZE: &str = "https://auth.x.ai/oauth2/authorize";
const GROK_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";

/// Where a sign-in goes: the providers' own, or a test's.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    /// The callback's port; `None`: any free one.
    pub port: Option<u16>,
}

impl Endpoints {
    pub fn of(subscription: Subscription) -> Endpoints {
        match subscription {
            Subscription::ChatGpt => Endpoints {
                authorize: CHATGPT_AUTHORIZE.into(),
                token: subscription.token_url().into(),
                port: Some(CHATGPT_PORT),
            },
            Subscription::Grok => {
                Endpoints { authorize: GROK_AUTHORIZE.into(), token: subscription.token_url().into(), port: None }
            }
        }
    }
}

/// A sign-in waiting for the browser.
pub struct Login {
    pub subscription: Subscription,
    /// The page to open.
    pub url: String,
    token_url: String,
    redirect_uri: String,
    path: &'static str,
    verifier: String,
    state: String,
    listener: TcpListener,
}

fn random(bytes: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; bytes];
    getrandom::getrandom(&mut out).map_err(|e| anyhow!("no randomness: {e}"))?;
    Ok(out)
}

/// Base64url without padding.
fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |acc, b| (acc << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// A PKCE verifier and its S256 challenge.
pub(crate) fn pkce() -> Result<(String, String)> {
    let verifier = base64url(&random(32)?);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

/// `bytes` random bytes in hex.
pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    Ok(hex(&random(bytes)?))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Form and query encoding.
fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(b) => {
                    out.push(b);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn query(pairs: &[(&str, &str)]) -> String {
    pairs.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

/// Starts a sign-in to `subscription`: the callback is listening when this
/// returns.
pub async fn begin(subscription: Subscription) -> Result<Login> {
    begin_with(subscription, Endpoints::of(subscription)).await
}

pub async fn begin_with(subscription: Subscription, endpoints: Endpoints) -> Result<Login> {
    let listener = TcpListener::bind(("127.0.0.1", endpoints.port.unwrap_or(0))).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            anyhow!(
                "port {} is already in use — close any other sign-in still running and try again",
                endpoints.port.unwrap_or(0)
            )
        } else {
            anyhow!("the local sign-in callback could not start: {error}")
        }
    })?;
    let port = listener.local_addr()?.port();
    let (verifier, challenge) = pkce()?;
    let state = hex(&random(16)?);
    let (redirect_uri, path) = match subscription {
        // The ChatGPT client allows exactly this address.
        Subscription::ChatGpt => (format!("http://localhost:{port}/auth/callback"), "/auth/callback"),
        Subscription::Grok => (format!("http://127.0.0.1:{port}/callback"), "/callback"),
    };
    let params = match subscription {
        Subscription::ChatGpt => query(&[
            ("response_type", "code"),
            ("client_id", subscription.client_id()),
            ("redirect_uri", &redirect_uri),
            ("scope", CHATGPT_SCOPE),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
        ]),
        Subscription::Grok => {
            let nonce = hex(&random(16)?);
            query(&[
                ("response_type", "code"),
                ("client_id", subscription.client_id()),
                ("redirect_uri", &redirect_uri),
                ("scope", GROK_SCOPE),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                ("nonce", &nonce),
                ("referrer", "grok-build"),
            ])
        }
    };
    Ok(Login {
        subscription,
        url: format!("{}?{params}", endpoints.authorize),
        token_url: endpoints.token,
        redirect_uri,
        path,
        verifier,
        state,
        listener,
    })
}

/// What a finished sign-in says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub subscription: Subscription,
    /// The ChatGPT account.
    pub account: Option<String>,
}

const PAGE_OK: &str = "<html><body style=\"font-family:system-ui;padding:40px\"><h2>Signed in</h2><p>You can close this tab and return to Bubble.</p></body></html>";

impl Login {
    /// Waits for the browser, exchanges the code and saves the sign-in in
    /// `auth`.
    pub async fn finish(self, client: &reqwest::Client, auth: &AuthFile) -> Result<SignedIn> {
        let code = tokio::time::timeout(WAIT, self.callback())
            .await
            .map_err(|_| anyhow!("the sign-in timed out after {} minutes", WAIT.as_secs() / 60))??;
        let response = client
            .post(&self.token_url)
            .timeout(Duration::from_secs(30))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", self.subscription.client_id()),
                ("code_verifier", self.verifier.as_str()),
            ])
            .send()
            .await
            .context("exchanging the sign-in code")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("the token exchange failed: {status} {}", body.chars().take(300).collect::<String>());
        }
        let answer: Value = serde_json::from_str(&body).context("the token answer cannot be read")?;
        let entry = entry_from(self.subscription, &answer)?;
        let account = entry["accountId"].as_str().map(str::to_owned);
        let path = auth.clone();
        let subscription = self.subscription;
        tokio::task::spawn_blocking(move || path.save(subscription, entry)).await??;
        Ok(SignedIn { subscription, account })
    }

    /// The code the browser brings back to the callback.
    async fn callback(&self) -> Result<String> {
        wait_for_code(&self.listener, self.path, &self.state).await
    }
}

/// Waits on `listener` for the browser to come back to `path` with a code
/// for `state`; other requests are answered and ignored.
pub(crate) async fn wait_for_code(listener: &TcpListener, path: &str, state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
            match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => request.extend_from_slice(&buffer[..n]),
                Ok(Err(_)) => break,
            }
        }
        let text = String::from_utf8_lossy(&request);
        let target = text.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("/");
        let (request_path, query) = target.split_once('?').unwrap_or((target, ""));
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (decode(k), decode(v)))
            .collect();
        let reply = |status: &str, body: &str| {
            format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        if request_path != path {
            let _ = stream.write_all(reply("404 Not Found", "Not found").as_bytes()).await;
            continue;
        }
        if let Some(error) = params.get("error") {
            let detail = params.get("error_description").unwrap_or(error).clone();
            let page = format!("<html><body><h2>Sign-in failed</h2><p>{}</p></body></html>", html_escape(&detail));
            let _ = stream.write_all(reply("400 Bad Request", &page).as_bytes()).await;
            bail!("the sign-in was refused: {detail}");
        }
        let Some(code) = params.get("code") else {
            let _ = stream.write_all(reply("400 Bad Request", "No code").as_bytes()).await;
            continue;
        };
        if params.get("state").map(String::as_str) != Some(state) {
            let _ = stream.write_all(reply("400 Bad Request", "State mismatch").as_bytes()).await;
            bail!("the sign-in came back with the wrong state; try again");
        }
        let _ = stream.write_all(reply("200 OK", PAGE_OK).as_bytes()).await;
        return Ok(code.clone());
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The `auth.json` entry for a token answer, as the TypeScript CLI writes it.
fn entry_from(subscription: Subscription, answer: &Value) -> Result<Value> {
    let access = answer["access_token"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("no access token came back"))?;
    let refresh = answer["refresh_token"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("no refresh token came back"))?;
    let expires_in = answer["expires_in"].as_i64().filter(|s| *s > 0).unwrap_or(3600);
    let mut entry = json!({
        "type": "oauth",
        "accessToken": access,
        "refreshToken": refresh,
        "expiresAt": now_ms() + expires_in * 1000,
    });
    if subscription == Subscription::ChatGpt {
        let id_token = answer["id_token"].as_str();
        if let Some(id_token) = id_token {
            entry["idToken"] = json!(id_token);
        }
        if let Some(account) = id_token.and_then(id_token_account).or_else(|| account_id_in(access)) {
            entry["accountId"] = json!(account);
        }
    }
    Ok(entry)
}

/// The ChatGPT account in an id token's claims (its subject as a last resort).
fn id_token_account(jwt: &str) -> Option<String> {
    if let Some(account) = account_id_in(jwt) {
        return Some(account);
    }
    let payload = jwt.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(&crate::auth::base64_url_decode(payload)?).ok()?;
    claims["sub"].as_str().map(str::to_owned)
}

/// Opens `url` in the default browser; whether a browser could be asked.
pub fn open_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    crate::detach(&mut command);
    command.spawn().is_ok()
}

/// Takes the sign-in of the official Grok CLI (Bubble's isolated profile
/// of it, or `~/.grok`), so no browser is needed; whether there was one.
pub fn import_grok_cli(auth: &AuthFile, bubble_home: &Path, home: &Path) -> Result<bool> {
    let candidates = [bubble_home.join("runtimes/grok/grok-home/auth.json"), home.join(".grok/auth.json")];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else { continue };
        for (key, value) in map {
            if !key.starts_with("https://auth.x.ai::") {
                continue;
            }
            let (Some(access), Some(refresh)) = (
                value["key"].as_str().filter(|s| !s.is_empty()),
                value["refresh_token"].as_str().filter(|s| !s.is_empty()),
            ) else {
                continue;
            };
            // An unreadable expiry means the first request refreshes.
            let expires = value["expires_at"].as_str().and_then(crate::time::parse_iso_millis).unwrap_or(0);
            auth.save(
                Subscription::Grok,
                json!({ "type": "oauth", "accessToken": access, "refreshToken": refresh, "expiresAt": expires }),
            )?;
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_match_the_standards() {
        assert_eq!(base64url(b"hello?"), "aGVsbG8_");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
        assert_eq!(decode("a%20b+c%3D"), "a b c=");
        // The PKCE example of RFC 7636.
        let challenge = base64url(&Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-login-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A token endpoint that answers one exchange.
    async fn token_server(answer: Value) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                        .unwrap_or(0);
                    if body.len() >= length {
                        let reply = answer.to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}",
                            reply.len()
                        );
                        stream.write_all(response.as_bytes()).await.unwrap();
                        return body.to_owned();
                    }
                }
            }
        });
        (url, handle)
    }

    #[tokio::test]
    async fn a_browser_sign_in_is_exchanged_and_saved() {
        let dir = temp("flow");
        let auth = AuthFile::at(dir.join("auth.json"));
        std::fs::write(dir.join("auth.json"), r#"{"other":{"apiKey":"keep"}}"#).unwrap();
        let (token_url, exchange) = token_server(json!({ "access_token": "at", "refresh_token": "rt", "expires_in": 600 })).await;
        let login = begin_with(Subscription::Grok, Endpoints { authorize: "https://example.test/authorize".into(), token: token_url, port: None })
            .await
            .unwrap();
        assert!(login.url.contains("code_challenge_method=S256") && login.url.contains("referrer=grok-build"));
        let state = login.url.split("state=").nth(1).unwrap().split('&').next().unwrap().to_owned();
        let redirect = decode(login.url.split("redirect_uri=").nth(1).unwrap().split('&').next().unwrap());
        let client = reqwest::Client::new();
        let finishing = tokio::spawn({
            let auth = auth.clone();
            let client = client.clone();
            async move { login.finish(&client, &auth).await }
        });
        // A stray request first, then the browser coming back.
        let _ = client.get(redirect.replace("/callback", "/favicon.ico")).send().await;
        let page = client.get(format!("{redirect}?code=the%20code&state={state}")).send().await.unwrap();
        assert!(page.text().await.unwrap().contains("Signed in"));
        let signed = finishing.await.unwrap().unwrap();
        assert_eq!(signed.subscription, Subscription::Grok);
        let sent = exchange.await.unwrap();
        assert!(sent.contains("grant_type=authorization_code") && sent.contains("code=the+code") && sent.contains("code_verifier="));
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("auth.json")).unwrap()).unwrap();
        assert_eq!(saved["grok"]["accessToken"], "at");
        assert_eq!(saved["grok"]["refreshToken"], "rt");
        assert_eq!(saved["other"]["apiKey"], "keep", "other entries stay");
        assert!(auth.remove(Subscription::Grok).unwrap());
        assert!(!auth.signed_in(Subscription::Grok));
        assert!(!auth.remove(Subscription::Grok).unwrap());
    }

    #[tokio::test]
    async fn a_wrong_state_is_refused() {
        let login = begin_with(
            Subscription::Grok,
            Endpoints { authorize: "https://example.test/a".into(), token: "http://127.0.0.1:9/t".into(), port: None },
        )
        .await
        .unwrap();
        let redirect = decode(login.url.split("redirect_uri=").nth(1).unwrap().split('&').next().unwrap());
        let auth = AuthFile::at(temp("state").join("auth.json"));
        let finishing = tokio::spawn(async move { login.finish(&reqwest::Client::new(), &auth).await });
        let _ = reqwest::get(format!("{redirect}?code=x&state=forged")).await;
        assert!(finishing.await.unwrap().unwrap_err().to_string().contains("wrong state"));
    }

    #[test]
    fn a_grok_cli_sign_in_is_imported() {
        let dir = temp("import");
        std::fs::create_dir_all(dir.join(".grok")).unwrap();
        std::fs::write(
            dir.join(".grok/auth.json"),
            r#"{"https://auth.x.ai::abc":{"key":"access","refresh_token":"refresh","expires_at":"2026-11-09T10:00:00Z"}}"#,
        )
        .unwrap();
        let auth = AuthFile::at(dir.join("bubble/auth.json"));
        assert!(import_grok_cli(&auth, &dir.join("bubble"), &dir).unwrap());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("bubble/auth.json")).unwrap()).unwrap();
        assert_eq!(saved["grok"]["accessToken"], "access");
        assert!(saved["grok"]["expiresAt"].as_i64().unwrap() > 1_700_000_000_000);
        assert!(!import_grok_cli(&AuthFile::at(dir.join("x/auth.json")), &dir.join("none"), &dir.join("none")).unwrap());
    }
}

