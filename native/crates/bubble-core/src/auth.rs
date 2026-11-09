//! Subscription sign-ins (ChatGPT, Grok) stored in Bubble's `auth.json`, the
//! file the TypeScript CLI writes: read the tokens, refresh them before they
//! expire, and write them back under the same cross-process lock, leaving
//! every other entry as it was.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::config::bubble_home;

/// Refresh this long before a token expires.
const REFRESH_GRACE_MS: i64 = 5 * 60 * 1000;
const LOCK_STALE: Duration = Duration::from_secs(30);
const LOCK_WAIT: Duration = Duration::from_secs(90);
const LOCK_POLL: Duration = Duration::from_millis(50);

/// A signed-in subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subscription {
    ChatGpt,
    Grok,
}

impl Subscription {
    /// Its key in auth.json, and the provider id it appears as.
    pub fn key(self) -> &'static str {
        match self {
            Subscription::ChatGpt => "openai",
            Subscription::Grok => "grok",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "openai" | "openai-codex" => Some(Subscription::ChatGpt),
            "grok" => Some(Subscription::Grok),
            _ => None,
        }
    }

    pub(crate) fn token_url(self) -> &'static str {
        match self {
            Subscription::ChatGpt => "https://auth.openai.com/oauth/token",
            Subscription::Grok => "https://auth.x.ai/oauth2/token",
        }
    }

    pub(crate) fn client_id(self) -> &'static str {
        match self {
            Subscription::ChatGpt => "app_EMoamEEZ73f0CkXaXp7hrann",
            Subscription::Grok => "b1a00492-073a-47ea-816f-4c329264a828",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Subscription::ChatGpt => "ChatGPT",
            Subscription::Grok => "Grok",
        }
    }
}

/// A token ready to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub access_token: String,
    /// The ChatGPT account the token acts for.
    pub account_id: Option<String>,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Bubble's auth.json.
#[derive(Clone, Debug)]
pub struct AuthFile {
    path: PathBuf,
}

impl Default for AuthFile {
    fn default() -> Self {
        AuthFile { path: bubble_home().join("auth.json") }
    }
}

impl AuthFile {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        AuthFile { path: path.into() }
    }

    fn read(&self) -> Result<Map<String, Value>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str::<Value>(&text)
                .with_context(|| format!("reading {}", self.path.display()))?
                .as_object()
                .cloned()
                .unwrap_or_default()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(error) => Err(error).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    /// The entry of `subscription`; ChatGPT also answers to its old key.
    fn entry(map: &Map<String, Value>, subscription: Subscription) -> Option<Value> {
        let keys: &[&str] = match subscription {
            Subscription::ChatGpt => &["openai", "openai-codex"],
            Subscription::Grok => &["grok"],
        };
        keys.iter().find_map(|k| map.get(*k).filter(|v| v["accessToken"].is_string()).cloned())
    }

    /// Stores a new sign-in for `subscription` (its `oauth` entry), under
    /// the refresh lock; other entries stay as they were.
    pub fn save(&self, subscription: Subscription, entry: Value) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _lock = Lock::acquire(&self.path)?;
        let mut map = self.read()?;
        if subscription == Subscription::ChatGpt {
            map.remove("openai-codex");
        }
        map.insert(subscription.key().to_owned(), entry);
        self.write(&map)
    }

    /// Forgets the sign-in of `subscription` on this device; whether there
    /// was one.
    pub fn remove(&self, subscription: Subscription) -> Result<bool> {
        let _lock = Lock::acquire(&self.path)?;
        let mut map = self.read()?;
        let keys: &[&str] = match subscription {
            Subscription::ChatGpt => &["openai", "openai-codex"],
            Subscription::Grok => &["grok"],
        };
        let mut removed = false;
        for key in keys {
            removed |= map.remove(*key).is_some();
        }
        if removed {
            self.write(&map)?;
        }
        Ok(removed)
    }

    /// Whether the person signed in to `subscription`.
    pub fn signed_in(&self, subscription: Subscription) -> bool {
        self.read().ok().and_then(|map| Self::entry(&map, subscription)).is_some()
    }

    /// A usable token, refreshed first when it is about to expire (or when
    /// `force`d after the provider said it had).
    pub async fn token(&self, client: &reqwest::Client, subscription: Subscription, force: bool) -> Result<Token> {
        let map = self.read()?;
        let entry = Self::entry(&map, subscription)
            .ok_or_else(|| anyhow!("not signed in to {}; sign in with the Bubble CLI (/login)", subscription.label()))?;
        if !force && !expiring(&entry) {
            return Ok(token_of(&entry));
        }
        let seen_refresh = entry["refreshToken"].clone();
        let path = self.path.clone();
        let lock = tokio::task::spawn_blocking(move || Lock::acquire(&path)).await??;
        // Another process may have refreshed while this one waited: its
        // token is then the one to use.
        let map = self.read()?;
        let entry = Self::entry(&map, subscription).ok_or_else(|| anyhow!("the {} sign-in is gone", subscription.label()))?;
        if !expiring(&entry) && (!force || entry["refreshToken"] != seen_refresh) {
            drop(lock);
            return Ok(token_of(&entry));
        }
        let refreshed = refresh(client, subscription, &entry).await;
        let result = match refreshed {
            Ok(next) => {
                let mut map = self.read()?;
                map.insert(subscription.key().to_owned(), next.clone());
                self.write(&map)?;
                Ok(token_of(&next))
            }
            Err(error) => Err(error),
        };
        drop(lock);
        result
    }

    fn write(&self, map: &Map<String, Value>) -> Result<()> {
        let text = serde_json::to_string_pretty(&Value::Object(map.clone()))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn expiring(entry: &Value) -> bool {
    entry["expiresAt"].as_i64().map(|at| now_ms() >= at - REFRESH_GRACE_MS).unwrap_or(true)
}

fn token_of(entry: &Value) -> Token {
    let access_token = entry["accessToken"].as_str().unwrap_or_default().to_owned();
    let account_id = entry["accountId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| account_id_in(&access_token));
    Token { access_token, account_id }
}

/// The ChatGPT account id in a token's claims.
pub fn account_id_in(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64_url_decode(payload)?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let auth = &claims["https://api.openai.com/auth"];
    auth["chatgpt_account_id"].as_str().or_else(|| auth["account_id"].as_str()).map(str::to_owned)
}

pub(crate) fn base64_url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut out = Vec::new();
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

async fn refresh(client: &reqwest::Client, subscription: Subscription, entry: &Value) -> Result<Value> {
    let refresh_token = entry["refreshToken"].as_str().ok_or_else(|| anyhow!("no refresh token"))?;
    let response = client
        .post(subscription.token_url())
        .timeout(Duration::from_secs(30))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", subscription.client_id()),
        ])
        .send()
        .await
        .with_context(|| format!("refreshing the {} sign-in", subscription.label()))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 400 || status.as_u16() == 401 || body.contains("refresh_token_reused") || body.contains("invalid_grant") {
            bail!("the {} sign-in is no longer valid; sign in again with the Bubble CLI (/login)", subscription.label());
        }
        bail!("refreshing the {} sign-in failed: {status}", subscription.label());
    }
    let answer: Value = serde_json::from_str(&body).context("the token answer cannot be read")?;
    let access = answer["access_token"].as_str().ok_or_else(|| anyhow!("the token answer has no access token"))?;
    let expires_in = answer["expires_in"].as_i64().filter(|s| *s > 0).unwrap_or(3600);
    let mut next = entry.clone();
    next["type"] = json!("oauth");
    next["accessToken"] = json!(access);
    if let Some(refresh) = answer["refresh_token"].as_str() {
        next["refreshToken"] = json!(refresh);
    }
    next["expiresAt"] = json!(now_ms() + expires_in * 1000);
    if let Some(id_token) = answer["id_token"].as_str() {
        next["idToken"] = json!(id_token);
    }
    if subscription == Subscription::ChatGpt && next["accountId"].as_str().is_none_or(str::is_empty) {
        if let Some(id) = account_id_in(access) {
            next["accountId"] = json!(id);
        }
    }
    Ok(next)
}

/// `auth.json.lock`, the lock the TypeScript CLI takes for a refresh: made
/// exclusively, broken when older than 30 s.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(auth: &Path) -> Result<Self> {
        let path = auth.with_extension("json.lock");
        let started = std::time::Instant::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Lock { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|modified| modified.elapsed().unwrap_or_default() > LOCK_STALE)
                        .unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() > LOCK_WAIT {
                        bail!("another Bubble is refreshing the sign-in; try again");
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(error) => return Err(error).with_context(|| format!("locking {}", path.display())),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: Value) -> String {
        fn encode(bytes: &[u8]) -> String {
            const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let n = chunk.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32) << (8 * (3 - chunk.len()));
                for i in 0..=chunk.len() {
                    out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            out
        }
        format!("{}.{}.sig", encode(b"{\"alg\":\"none\"}"), encode(claims.to_string().as_bytes()))
    }

    #[test]
    fn account_ids_come_from_the_token_claims() {
        let token = jwt(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct-123" } }));
        assert_eq!(account_id_in(&token).as_deref(), Some("acct-123"));
        assert_eq!(account_id_in("not.a.jwt"), None);
    }

    fn file(content: Value) -> (AuthFile, PathBuf) {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("bubble-auth-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        std::fs::write(&path, content.to_string()).unwrap();
        (AuthFile::at(&path), dir)
    }

    #[tokio::test]
    async fn a_fresh_token_is_used_as_is() {
        let (auth, dir) = file(json!({
            "openai": { "type": "oauth", "accessToken": "a1", "refreshToken": "r1", "expiresAt": now_ms() + 3_600_000, "accountId": "acct" },
            "other": { "keep": true }
        }));
        assert!(auth.signed_in(Subscription::ChatGpt));
        assert!(!auth.signed_in(Subscription::Grok));
        let client = reqwest::Client::new();
        let token = auth.token(&client, Subscription::ChatGpt, false).await.unwrap();
        assert_eq!(token, Token { access_token: "a1".into(), account_id: Some("acct".into()) });
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn an_expired_token_without_a_reachable_server_fails_clearly() {
        let (auth, dir) = file(json!({ "grok": { "type": "oauth", "accessToken": "g", "refreshToken": "r", "expiresAt": 1 } }));
        let client = reqwest::Client::builder().timeout(Duration::from_millis(1)).build().unwrap();
        let error = auth.token(&client, Subscription::Grok, false).await.unwrap_err();
        assert!(error.to_string().contains("Grok"), "{error}");
        assert!(!dir.join("auth.json.lock").exists(), "the lock is released");
        std::fs::remove_dir_all(dir).ok();
    }
}
