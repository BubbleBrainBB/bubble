//! Reads the providers and the default model from Bubble's own configuration
//! (`~/.bubble/config.json`), the same file the TypeScript CLI writes, so a
//! person configures a provider once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::auth::{AuthFile, Subscription};

/// Where Bubble keeps its data: `$BUBBLE_HOME`, else `~/.bubble`.
pub fn bubble_home() -> PathBuf {
    if let Some(home) = std::env::var_os("BUBBLE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".bubble")
}

/// The wire protocol a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// OpenAI Chat Completions (`POST {baseURL}/chat/completions`), which
    /// most providers implement.
    OpenAiChat,
    /// Anthropic Messages (`POST {baseURL}/v1/messages`).
    AnthropicMessages,
    /// An OpenAI Responses API (`POST …/responses`).
    Responses(Dialect),
    /// Google's Gemini API (`POST …/models/{model}:streamGenerateContent`).
    Gemini,
}

/// Which Responses API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// ChatGPT's Codex backend, signed in with a ChatGPT subscription.
    Codex,
    /// Volcano Engine Ark (Doubao).
    Ark,
    /// OpenAI's own and compatible ones (opencode-zen).
    OpenAi,
}

pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub const GROK_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const GROK_CLIENT_VERSION: &str = "1.0.13";
const ARK_BASE_URL: &str = "https://ark.cn-beijing.volces.com/api/v3";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "baseURL", default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub protocol: Option<String>,
    /// Extra request headers (a coding plan's client-identity gate).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

fn enabled_by_default() -> bool {
    true
}

impl ProviderConfig {
    pub fn protocol(&self) -> Result<Protocol> {
        let ark_default = self.id == "doubao"
            && self.base_url.as_deref().map(|u| u.trim_end_matches('/')) == Some(ARK_BASE_URL);
        match self.protocol.as_deref() {
            None if ark_default => Ok(Protocol::Responses(Dialect::Ark)),
            None | Some("openai") | Some("openai-chat") | Some("openai-completions") => Ok(Protocol::OpenAiChat),
            Some("anthropic-messages") | Some("anthropic") => Ok(Protocol::AnthropicMessages),
            Some("ark-responses") => Ok(Protocol::Responses(Dialect::Ark)),
            Some("openai-responses") => Ok(Protocol::Responses(Dialect::OpenAi)),
            // `ai-sdk` is the TypeScript CLI's name for its Gemini protocol.
            Some("ai-sdk") | Some("gemini") | Some("google") | Some("google-generative-ai") => Ok(Protocol::Gemini),
            Some(other) => bail!(
                "provider {} uses the {other} protocol, which Bubble Native does not support yet",
                self.id
            ),
        }
    }

    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub default_thinking_level: Option<String>,
    #[serde(default)]
    pub recent_models: Vec<String>,
    /// More folders to look for skills in.
    #[serde(default)]
    pub skill_paths: Vec<String>,
    #[serde(default)]
    pub skills: SkillSettings,
    /// Subagent categories on top of the built-in ones.
    #[serde(default)]
    pub agent_categories: std::collections::BTreeMap<String, crate::routing::Category>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSettings {
    /// Skills switched off.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// The budget of the system prompt's skill list.
    #[serde(default)]
    pub catalog_chars: Option<usize>,
}

/// A model chosen by reference (`provider:model`), with what calling it needs.
#[derive(Clone, Debug)]
pub struct ResolvedModel {
    pub provider: ProviderConfig,
    pub model: String,
    pub protocol: Protocol,
    pub base_url: String,
    /// Empty for a subscription, whose token is fetched for each request.
    pub api_key: String,
    pub subscription: Option<Subscription>,
}

impl ResolvedModel {
    /// `provider:model`, as config and the UI write it.
    pub fn reference(&self) -> String {
        format!("{}:{}", self.provider.id, self.model)
    }
}

impl Config {
    /// Loads `config.json` from Bubble's home; a missing file is an empty
    /// configuration.
    pub fn load() -> Result<Self> {
        Self::load_from(&bubble_home().join("config.json"))
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("reading {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(serde_json::from_str(text)?)
    }

    /// The default model's reference: `defaultModel` when it names its
    /// provider (`kimi-for-coding:k3`), else `defaultProvider:defaultModel`.
    pub fn default_model_reference(&self) -> Option<String> {
        let model = self.default_model.as_deref()?.trim();
        if model.is_empty() {
            return None;
        }
        if let Some((provider, _)) = model.split_once(':') {
            if self.provider(provider).is_some() {
                return Some(model.to_owned());
            }
        }
        let provider = self.default_provider.as_deref()?;
        Some(format!("{provider}:{model}"))
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Resolves `provider:model` against the configured providers and the
    /// subscriptions signed in to.
    pub fn resolve(&self, reference: &str) -> Result<ResolvedModel> {
        self.resolve_with(reference, &AuthFile::default())
    }

    pub fn resolve_with(&self, reference: &str, auth: &AuthFile) -> Result<ResolvedModel> {
        let (provider_id, model) = reference
            .split_once(':')
            .ok_or_else(|| anyhow!("model {reference:?} does not name its provider (provider:model)"))?;
        let keyed = self.provider(provider_id).is_some_and(|p| p.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()));
        if let Some(subscription) = Subscription::from_key(provider_id).filter(|_| !keyed) {
            if !auth.signed_in(subscription) {
                bail!("not signed in to {}; sign in with the Bubble CLI (/login)", subscription.label());
            }
            return Ok(subscription_model(subscription, model));
        }
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| anyhow!("no provider {provider_id:?} in config.json"))?;
        if !provider.enabled {
            bail!("provider {provider_id} is turned off");
        }
        let protocol = provider.protocol()?;
        let base_url = provider
            .base_url
            .clone()
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| anyhow!("provider {provider_id} has no baseURL"))?;
        let api_key = provider
            .api_key
            .clone()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow!("provider {provider_id} has no API key; sign-in providers come later"))?;
        Ok(ResolvedModel {
            provider: provider.clone(),
            model: model.to_owned(),
            protocol,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            subscription: None,
        })
    }

    /// The models a person can pick from: the default and the recent ones
    /// that resolve, then those of the subscriptions signed in to.
    pub fn selectable_models(&self) -> Vec<String> {
        self.selectable_models_with(&AuthFile::default())
    }

    pub fn selectable_models_with(&self, auth: &AuthFile) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        let subscriptions = [Subscription::ChatGpt, Subscription::Grok]
            .into_iter()
            .filter(|s| auth.signed_in(*s))
            .flat_map(|s| crate::model::subscription_models(s).into_iter().map(move |m| format!("{}:{m}", s.key())));
        for reference in self.default_model_reference().into_iter().chain(self.recent_models.iter().cloned()).chain(subscriptions) {
            if self.resolve_with(&reference, auth).is_ok() && !models.contains(&reference) {
                models.push(reference);
            }
        }
        models
    }
}

/// A model of a subscription: its built-in provider, signed in.
fn subscription_model(subscription: Subscription, model: &str) -> ResolvedModel {
    let (name, base_url, protocol, headers) = match subscription {
        Subscription::ChatGpt => ("ChatGPT", CODEX_BASE_URL, Protocol::Responses(Dialect::Codex), BTreeMap::new()),
        Subscription::Grok => (
            "Grok",
            GROK_BASE_URL,
            Protocol::OpenAiChat,
            BTreeMap::from([
                ("User-Agent".to_owned(), "grok-cli".to_owned()),
                ("x-grok-client-version".to_owned(), GROK_CLIENT_VERSION.to_owned()),
            ]),
        ),
    };
    ResolvedModel {
        provider: ProviderConfig {
            id: subscription.key().to_owned(),
            name: Some(name.to_owned()),
            base_url: Some(base_url.to_owned()),
            api_key: None,
            enabled: true,
            protocol: None,
            headers,
        },
        model: model.to_owned(),
        protocol,
        base_url: base_url.to_owned(),
        api_key: String::new(),
        subscription: Some(subscription),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "providers": [
        {"id": "kimi-for-coding", "name": "Kimi for Coding", "baseURL": "https://api.kimi.com/coding/v1/", "apiKey": "k-1", "enabled": true},
        {"id": "anthropic", "baseURL": "https://api.anthropic.com", "protocol": "anthropic-messages", "apiKey": "a-1"},
        {"id": "off", "baseURL": "https://off.example", "apiKey": "x", "enabled": false},
        {"id": "nokey", "baseURL": "https://nokey.example"}
      ],
      "defaultProvider": "openai",
      "defaultModel": "kimi-for-coding:k3",
      "recentModels": ["anthropic:claude-sonnet-5-5", "openai:gpt-5.5", "kimi-for-coding:k3"]
    }"#;

    #[test]
    fn the_default_model_can_name_its_provider() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(config.default_model_reference().as_deref(), Some("kimi-for-coding:k3"));
    }

    #[test]
    fn a_bare_default_model_takes_the_default_provider() {
        let config = Config::parse(r#"{"defaultProvider":"deepseek","defaultModel":"deepseek-chat"}"#).unwrap();
        assert_eq!(config.default_model_reference().as_deref(), Some("deepseek:deepseek-chat"));
    }

    #[test]
    fn resolving_picks_the_protocol_and_trims_the_url() {
        let config = Config::parse(SAMPLE).unwrap();
        let kimi = config.resolve("kimi-for-coding:k3").unwrap();
        assert_eq!(kimi.protocol, Protocol::OpenAiChat);
        assert_eq!(kimi.base_url, "https://api.kimi.com/coding/v1");
        let claude = config.resolve("anthropic:claude-sonnet-5-5").unwrap();
        assert_eq!(claude.protocol, Protocol::AnthropicMessages);
        assert_eq!(claude.reference(), "anthropic:claude-sonnet-5-5");
    }

    #[test]
    fn unusable_providers_are_explained() {
        let config = Config::parse(SAMPLE).unwrap();
        assert!(config.resolve("off:m").unwrap_err().to_string().contains("turned off"));
        assert!(config.resolve("nokey:m").unwrap_err().to_string().contains("no API key"));
        assert!(config.resolve("missing:m").unwrap_err().to_string().contains("no provider"));
        assert!(config.resolve("bare").unwrap_err().to_string().contains("provider:model"));
    }

    fn no_sign_ins() -> AuthFile {
        AuthFile::at(std::env::temp_dir().join("bubble-no-auth-here.json"))
    }

    #[test]
    fn selectable_models_skip_what_cannot_run() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(config.selectable_models_with(&no_sign_ins()), ["kimi-for-coding:k3", "anthropic:claude-sonnet-5-5"]);
    }

    #[test]
    fn subscriptions_resolve_when_signed_in() {
        let dir = std::env::temp_dir().join(format!("bubble-config-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        std::fs::write(&path, r#"{"openai":{"type":"oauth","accessToken":"a","refreshToken":"r","expiresAt":1}}"#).unwrap();
        let auth = AuthFile::at(&path);
        let config = Config::parse(SAMPLE).unwrap();
        let codex = config.resolve_with("openai:gpt-5.5", &auth).unwrap();
        assert_eq!(codex.protocol, Protocol::Responses(Dialect::Codex));
        assert_eq!(codex.subscription, Some(Subscription::ChatGpt));
        assert!(config.resolve_with("grok:grok-4.5", &auth).unwrap_err().to_string().contains("not signed in to Grok"));
        let models = config.selectable_models_with(&auth);
        assert!(models.contains(&"openai:gpt-5.5".to_owned()), "{models:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn doubao_on_ark_and_responses_protocols_resolve() {
        let config = Config::parse(r#"{"providers":[
          {"id":"doubao","baseURL":"https://ark.cn-beijing.volces.com/api/v3","apiKey":"k"},
          {"id":"opencode-zen","baseURL":"https://opencode.ai/zen/v1","apiKey":"k","protocol":"openai-responses"}
        ]}"#).unwrap();
        assert_eq!(config.resolve("doubao:m").unwrap().protocol, Protocol::Responses(Dialect::Ark));
        assert_eq!(config.resolve("opencode-zen:m").unwrap().protocol, Protocol::Responses(Dialect::OpenAi));
    }
}
