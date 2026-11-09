//! Signing in to HTTP MCP servers that require OAuth (the MCP authorization
//! spec): a server that answers 401 names its protected-resource metadata,
//! which names an authorization server; Bubble registers itself there
//! (dynamic client registration), signs in in the browser with PKCE, and
//! keeps the tokens in `~/.bubble/mcp-auth.json`, refreshing them as they
//! expire. Requests then carry the bearer token.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

use crate::auth::now_ms;
use crate::config::bubble_home;
use crate::login::{pkce, query, random_hex, wait_for_code};

/// What a server's 401 said, by server address: where its metadata is.
static CHALLENGES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Refresh this long before a token expires.
const GRACE_MS: i64 = 60_000;

/// The marker of a sign-in error, so status lines can say what to do.
pub const NEEDS_SIGN_IN: &str = "needs sign-in";

fn store_path() -> PathBuf {
    std::env::var_os("BUBBLE_MCP_AUTH_FILE").map(PathBuf::from).unwrap_or_else(|| bubble_home().join("mcp-auth.json"))
}

fn read_store() -> Map<String, Value> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_store(map: &Map<String, Value>) -> Result<()> {
    let path = store_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(map.clone()))?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Notes a server's 401 (its `WWW-Authenticate`) for the sign-in.
pub fn challenged(server_url: &str, www_authenticate: Option<&str>) {
    if let Ok(mut challenges) = CHALLENGES.lock() {
        challenges.get_or_insert_with(HashMap::new).insert(server_url.to_owned(), www_authenticate.unwrap_or_default().to_owned());
    }
}

/// Whether a sign-in is stored for `server_url`.
pub fn signed_in(server_url: &str) -> bool {
    read_store().contains_key(server_url)
}

/// Forgets the sign-in for `server_url`; whether there was one.
pub fn forget(server_url: &str) -> Result<bool> {
    let mut store = read_store();
    let removed = store.remove(server_url).is_some();
    if removed {
        write_store(&store)?;
    }
    Ok(removed)
}

/// A usable access token for `server_url`, refreshed first when it is about
/// to expire; `None` without a sign-in (or when refreshing fails).
pub async fn bearer(http: &reqwest::Client, server_url: &str) -> Option<String> {
    let store = read_store();
    let entry = store.get(server_url)?.clone();
    let expires = entry["expiresAt"].as_i64().unwrap_or(i64::MAX);
    if now_ms() < expires - GRACE_MS {
        return entry["accessToken"].as_str().map(str::to_owned);
    }
    let refresh = entry["refreshToken"].as_str()?;
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh.to_owned()),
        ("client_id", entry["clientId"].as_str()?.to_owned()),
    ];
    if let Some(resource) = entry["resource"].as_str() {
        form.push(("resource", resource.to_owned()));
    }
    if let Some(secret) = entry["clientSecret"].as_str() {
        form.push(("client_secret", secret.to_owned()));
    }
    let response = http.post(entry["tokenEndpoint"].as_str()?).timeout(Duration::from_secs(30)).form(&form).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let answer: Value = response.json().await.ok()?;
    let mut next = entry.clone();
    apply_tokens(&mut next, &answer)?;
    let mut store = read_store();
    store.insert(server_url.to_owned(), next.clone());
    write_store(&store).ok()?;
    next["accessToken"].as_str().map(str::to_owned)
}

fn apply_tokens(entry: &mut Value, answer: &Value) -> Option<()> {
    entry["accessToken"] = json!(answer["access_token"].as_str()?);
    if let Some(refresh) = answer["refresh_token"].as_str() {
        entry["refreshToken"] = json!(refresh);
    }
    entry["expiresAt"] = match answer["expires_in"].as_i64() {
        Some(seconds) => json!(now_ms() + seconds * 1000),
        None => Value::Null,
    };
    Some(())
}

/// `resource_metadata="…"` in a `WWW-Authenticate` header.
fn metadata_hint(header: &str) -> Option<String> {
    let at = header.find("resource_metadata=")?;
    let rest = header[at + "resource_metadata=".len()..].trim_start_matches('"');
    Some(rest.split(['"', ',']).next()?.to_owned())
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<Value> {
    let response = http.get(url).header("accept", "application/json").timeout(Duration::from_secs(15)).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok().filter(Value::is_object)
}

/// The well-known address `name` for `url` (RFC 8414: the path goes after
/// the well-known part).
fn well_known(url: &Url, name: &str) -> Vec<String> {
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.push(format!("{origin}/.well-known/{name}{path}"));
    }
    out.push(format!("{origin}/.well-known/{name}"));
    out
}

/// Where to sign in for a server: its endpoints and what to ask for.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoints {
    pub authorization: String,
    pub token: String,
    pub registration: Option<String>,
    pub resource: String,
    pub scope: Option<String>,
}

/// Finds the authorization server of `server_url`.
pub async fn discover(http: &reqwest::Client, server_url: &str) -> Result<Endpoints> {
    let url = Url::parse(server_url).context("the MCP server address is not a URL")?;
    let hint = CHALLENGES.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.get(server_url).cloned())).and_then(|h| metadata_hint(&h));
    let mut resource_candidates: Vec<String> = hint.into_iter().collect();
    resource_candidates.extend(well_known(&url, "oauth-protected-resource"));
    let mut resource_metadata = None;
    for candidate in resource_candidates {
        if let Some(found) = get_json(http, &candidate).await {
            resource_metadata = Some(found);
            break;
        }
    }
    let resource = resource_metadata.as_ref().and_then(|m| m["resource"].as_str()).unwrap_or(server_url).to_owned();
    let scope = resource_metadata.as_ref().and_then(|m| m["scopes_supported"].as_array()).map(|scopes| {
        scopes.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
    });
    // The authorization server: as the metadata names it, else the
    // server's own origin (the older spec).
    let issuer = resource_metadata
        .as_ref()
        .and_then(|m| m["authorization_servers"][0].as_str())
        .and_then(|i| Url::parse(i).ok())
        .unwrap_or_else(|| url.clone());
    let mut metadata = None;
    for name in ["oauth-authorization-server", "openid-configuration"] {
        for candidate in well_known(&issuer, name) {
            if let Some(found) = get_json(http, &candidate).await {
                metadata = Some(found);
                break;
            }
        }
        if metadata.is_some() {
            break;
        }
    }
    let origin = issuer.origin().ascii_serialization();
    let field = |name: &str| metadata.as_ref().and_then(|m| m[name].as_str()).map(str::to_owned);
    Ok(Endpoints {
        authorization: field("authorization_endpoint").unwrap_or_else(|| format!("{origin}/authorize")),
        token: field("token_endpoint").unwrap_or_else(|| format!("{origin}/token")),
        registration: field("registration_endpoint").or_else(|| metadata.is_none().then(|| format!("{origin}/register"))),
        resource,
        scope: scope.filter(|s| !s.is_empty()),
    })
}

/// A sign-in waiting for the browser.
pub struct Pending {
    pub url: String,
    server_url: String,
    endpoints: Endpoints,
    client_id: String,
    client_secret: Option<String>,
    redirect_uri: String,
    verifier: String,
    state: String,
    listener: TcpListener,
}

/// Starts signing in to `server_url`: discovery, client registration, and
/// the browser address.
pub async fn begin(http: &reqwest::Client, server_url: &str) -> Result<Pending> {
    let endpoints = discover(http, server_url).await?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.context("the local sign-in callback could not start")?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
    let registration = endpoints
        .registration
        .clone()
        .ok_or_else(|| anyhow!("{server_url} does not let clients register themselves; it needs a client id Bubble does not have"))?;
    let registered: Value = http
        .post(&registration)
        .timeout(Duration::from_secs(30))
        .json(&json!({
            "client_name": "Bubble",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .context("registering Bubble with the authorization server")?
        .error_for_status()
        .context("registering Bubble with the authorization server")?
        .json()
        .await?;
    let client_id = registered["client_id"].as_str().ok_or_else(|| anyhow!("the registration gave no client id"))?.to_owned();
    let client_secret = registered["client_secret"].as_str().map(str::to_owned);
    let (verifier, challenge) = pkce()?;
    let state = random_hex(16)?;
    let mut pairs = vec![
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state.as_str()),
        ("resource", endpoints.resource.as_str()),
    ];
    if let Some(scope) = &endpoints.scope {
        pairs.push(("scope", scope.as_str()));
    }
    let separator = if endpoints.authorization.contains('?') { '&' } else { '?' };
    let url = format!("{}{separator}{}", endpoints.authorization, query(&pairs));
    Ok(Pending {
        url,
        server_url: server_url.to_owned(),
        endpoints,
        client_id,
        client_secret,
        redirect_uri,
        verifier,
        state,
        listener,
    })
}

impl Pending {
    /// Waits for the browser (five minutes at most) and keeps the tokens.
    pub async fn finish(self, http: &reqwest::Client) -> Result<()> {
        let code = tokio::time::timeout(Duration::from_secs(300), wait_for_code(&self.listener, "/callback", &self.state))
            .await
            .map_err(|_| anyhow!("the sign-in timed out after 5 minutes"))??;
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", self.redirect_uri.clone()),
            ("client_id", self.client_id.clone()),
            ("code_verifier", self.verifier.clone()),
            ("resource", self.endpoints.resource.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let response = http.post(&self.endpoints.token).timeout(Duration::from_secs(30)).form(&form).send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("the token exchange failed: {status} {}", body.chars().take(300).collect::<String>());
        }
        let answer: Value = serde_json::from_str(&body).context("the token answer cannot be read")?;
        let mut entry = json!({
            "clientId": self.client_id,
            "tokenEndpoint": self.endpoints.token,
            "resource": self.endpoints.resource,
        });
        if let Some(secret) = &self.client_secret {
            entry["clientSecret"] = json!(secret);
        }
        apply_tokens(&mut entry, &answer).ok_or_else(|| anyhow!("no access token came back"))?;
        let mut store = read_store();
        store.insert(self.server_url.clone(), entry);
        write_store(&store)?;
        if let Ok(mut challenges) = CHALLENGES.lock()
            && let Some(map) = challenges.as_mut()
        {
            map.remove(&self.server_url);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MCP server behind OAuth, with its own authorization server.
    const SERVER: &str = r#"
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def reply(self, code, body=None, headers=()):
        self.send_response(code)
        for k, v in headers: self.send_header(k, v)
        data = json.dumps(body).encode() if body is not None else b""
        if body is not None: self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        base = "http://127.0.0.1:%d" % self.server.server_port
        u = urlparse(self.path)
        if u.path == "/.well-known/oauth-protected-resource":
            self.reply(200, {"resource": base + "/mcp", "authorization_servers": [base], "scopes_supported": ["mcp"]})
        elif u.path == "/.well-known/oauth-authorization-server":
            self.reply(200, {"issuer": base, "authorization_endpoint": base + "/authorize", "token_endpoint": base + "/token", "registration_endpoint": base + "/register"})
        elif u.path == "/authorize":
            q = parse_qs(u.query)
            assert q["code_challenge_method"] == ["S256"] and q["resource"] == [base + "/mcp"] and q["client_id"] == ["c1"]
            self.reply(302, None, [("location", q["redirect_uri"][0] + "?code=abc&state=" + q["state"][0])])
        else:
            self.reply(404, {})
    def do_POST(self):
        base = "http://127.0.0.1:%d" % self.server.server_port
        body = self.rfile.read(int(self.headers.get("content-length", 0))).decode()
        if self.path == "/register":
            self.reply(201, {"client_id": "c1"})
        elif self.path == "/token":
            assert "code=abc" in body and "code_verifier=" in body, body
            self.reply(200, {"access_token": "good", "refresh_token": "r", "expires_in": 3600})
        elif self.path == "/mcp":
            if self.headers.get("authorization") != "Bearer good":
                self.reply(401, {"error": "unauthorized"}, [("www-authenticate", 'Bearer resource_metadata="%s/.well-known/oauth-protected-resource"' % base)])
                return
            m = json.loads(body)
            if "id" not in m:
                self.reply(202); return
            if m["method"] == "initialize":
                self.reply(200, {"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {"tools": {}}}})
            elif m["method"] == "tools/list":
                self.reply(200, {"jsonrpc": "2.0", "id": m["id"], "result": {"tools": [{"name": "issues"}]}})
            else:
                self.reply(200, {"jsonrpc": "2.0", "id": m["id"], "result": {}})
s = HTTPServer(("127.0.0.1", 0), H)
print(s.server_port, flush=True)
s.serve_forever()
"#;

    #[tokio::test]
    async fn a_protected_server_is_signed_in_to_and_then_connects() {
        use crate::mcp::{Hub, Scope, ScopedServer, ServerConfig, Status};
        let dir = std::env::temp_dir().join(format!("bubble-mcp-oauth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: only this test reads it.
        unsafe { std::env::set_var("BUBBLE_MCP_AUTH_FILE", dir.join("mcp-auth.json")) };
        let mut child = tokio::process::Command::new("python3")
            .args(["-c", SERVER])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut port = String::new();
        use tokio::io::AsyncBufReadExt;
        tokio::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
        let url = format!("http://127.0.0.1:{}/mcp", port.trim());
        let http = reqwest::Client::new();
        let hub = Hub::new();
        let configured = vec![ScopedServer {
            name: "tracker".into(),
            scope: Scope::User,
            config: ServerConfig::Http { url: url.clone(), headers: Default::default() },
        }];
        let servers = hub.connect(configured, &http).await;
        assert_eq!(servers[0].status(), Status::Failed(format!("{NEEDS_SIGN_IN}: /mcp login tracker")));

        let pending = servers[0].login(&http).await.unwrap();
        assert!(pending.url.contains("scope=mcp"));
        let browser = tokio::spawn({
            let url = pending.url.clone();
            async move { reqwest::get(url).await.unwrap().text().await.unwrap() }
        });
        pending.finish(&http).await.unwrap();
        assert!(browser.await.unwrap().contains("Signed in"));
        assert!(signed_in(&url));

        servers[0].reset().await;
        servers[0].ensure(&http).await;
        assert!(matches!(servers[0].status(), Status::Connected { tools, .. } if tools[0].name == "issues"));
        assert!(servers[0].logout().unwrap());
        assert!(!signed_in(&url));
    }

    #[test]
    fn challenges_name_their_metadata() {
        assert_eq!(
            metadata_hint(r#"Bearer error="invalid_token", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#).as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(metadata_hint("Bearer"), None);
        let url = Url::parse("https://example.com/mcp/v1").unwrap();
        assert_eq!(
            well_known(&url, "oauth-protected-resource"),
            ["https://example.com/.well-known/oauth-protected-resource/mcp/v1", "https://example.com/.well-known/oauth-protected-resource"]
        );
    }
}
