//! The models a subscription offers right now, asked of its service: their
//! context windows, reasoning levels and (ChatGPT) request shape. What is
//! found overrides the static tables in [`crate::model`] for this process.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::auth::{AuthFile, Subscription};
use crate::config::{CODEX_BASE_URL, GROK_BASE_URL, GROK_CLIENT_VERSION};
use crate::model::Level;

/// A model a service listed.
#[derive(Clone, Debug, PartialEq)]
pub struct Discovered {
    pub id: String,
    pub context_window: Option<u64>,
    pub levels: Vec<Level>,
    /// ChatGPT: takes the "responses lite" request shape.
    pub lite: bool,
}

static FOUND: RwLock<Option<HashMap<Subscription, Vec<Discovered>>>> = RwLock::new(None);

/// What was found for a subscription, in the service's order.
pub fn found(subscription: Subscription) -> Option<Vec<Discovered>> {
    FOUND.read().ok()?.as_ref()?.get(&subscription).cloned()
}

/// One found model.
pub fn lookup(provider: &str, model: &str) -> Option<Discovered> {
    let subscription = Subscription::from_key(provider)?;
    found(subscription)?.into_iter().find(|m| m.id == model)
}

pub fn remember(subscription: Subscription, models: Vec<Discovered>) {
    if let Ok(mut found) = FOUND.write() {
        found.get_or_insert_with(HashMap::new).insert(subscription, models);
    }
}

fn levels_of(values: impl Iterator<Item = String>) -> Vec<Level> {
    let mut levels: Vec<Level> = values.filter_map(|v| Level::parse(&v)).collect();
    levels.sort();
    levels.dedup();
    levels
}

/// Reads `GET /codex/models`: listed models, hidden ones left out.
pub fn parse_codex(json: &Value) -> Vec<Discovered> {
    json["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter(|m| m["visibility"].as_str() != Some("hide"))
                .filter_map(|m| {
                    let id = m["slug"].as_str().or_else(|| m["id"].as_str())?.to_owned();
                    let levels = levels_of(
                        m["supported_reasoning_levels"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|l| l["effort"].as_str().map(str::to_owned)),
                    );
                    Some(Discovered {
                        id,
                        context_window: m["context_window"].as_u64(),
                        levels,
                        lite: m["use_responses_lite"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reads Grok's `GET /models`.
pub fn parse_grok(json: &Value) -> Vec<Discovered> {
    json["data"]
        .as_array()
        .or_else(|| json["models"].as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    let id = m["id"].as_str()?.to_owned();
                    let mut levels = levels_of(
                        m["reasoning_efforts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|e| e["value"].as_str().or_else(|| e["id"].as_str()).map(str::to_owned)),
                    );
                    if levels.is_empty() {
                        levels.push(Level::Off);
                    }
                    Some(Discovered { id, context_window: m["context_window"].as_u64(), levels, lite: false })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Asks a subscription's service for its models and remembers them.
pub async fn discover(client: &reqwest::Client, subscription: Subscription) -> Result<Vec<Discovered>> {
    let token = AuthFile::default().token(client, subscription, false).await?;
    let request = match subscription {
        Subscription::ChatGpt => client
            .get(format!("{CODEX_BASE_URL}/codex/models?client_version=0.159.0"))
            .header("chatgpt-account-id", token.account_id.clone().unwrap_or_default())
            .header("originator", "codex_cli_rs")
            .header("user-agent", "codex_cli_rs/0.159.0 (bubble)"),
        Subscription::Grok => client
            .get(format!("{GROK_BASE_URL}/models"))
            .header("user-agent", "grok-cli")
            .header("x-grok-client-version", GROK_CLIENT_VERSION),
    };
    let response = request
        .bearer_auth(&token.access_token)
        .header("accept", "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("{} answered {} for its models", subscription.label(), response.status());
    }
    let json: Value = response.json().await?;
    let models = match subscription {
        Subscription::ChatGpt => parse_codex(&json),
        Subscription::Grok => parse_grok(&json),
    };
    if !models.is_empty() {
        remember(subscription, models.clone());
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_lists_skip_hidden_models() {
        let json = json!({ "models": [
            { "slug": "gpt-6.1-sol", "visibility": "list", "context_window": 500000, "use_responses_lite": true,
              "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "ultra" }] },
            { "slug": "gpt-reserve", "visibility": "hide" }
        ]});
        let found = parse_codex(&json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], Discovered { id: "gpt-6.1-sol".into(), context_window: Some(500_000), levels: vec![Level::Low, Level::Ultra], lite: true });
    }

    #[test]
    fn grok_lists_read_their_efforts() {
        let json = json!({ "data": [
            { "id": "grok-4.7", "context_window": 256000, "reasoning_efforts": [{ "value": "xhigh" }, { "value": "high" }, { "value": "low" }] },
            { "id": "grok-plain" }
        ]});
        let found = parse_grok(&json);
        assert_eq!(found[0].levels, [Level::Low, Level::High, Level::Xhigh]);
        assert_eq!(found[1].levels, [Level::Off]);
    }
}
