//! What Bubble knows about particular models: their context windows, the
//! reasoning levels they take, and how a level is asked for. Ported from
//! the TypeScript core (`model-catalog.ts`, `provider-transform.ts`,
//! `provider-anthropic.ts`); models it does not know run with defaults.

use std::borrow::Cow;

use serde_json::{Map, Value, json};

/// How hard a model should think, from none to the most it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl Level {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Level::Off,
            "minimal" => Level::Minimal,
            "low" => Level::Low,
            "medium" => Level::Medium,
            "high" => Level::High,
            "xhigh" => Level::Xhigh,
            "max" => Level::Max,
            "ultra" => Level::Ultra,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Minimal => "minimal",
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
            Level::Xhigh => "xhigh",
            Level::Max => "max",
            Level::Ultra => "ultra",
        }
    }
}

use Level::*;

const K3: &[Level] = &[Off, Low, High, Max];
const K3_EFFORT_ONLY: &[Level] = &[Low, High, Max];
const THINKING_ONLY: &[Level] = &[Medium];
const TOGGLE: &[Level] = &[Off, Medium];
const KIMI_PASSTHROUGH_K3: &[Level] = &[Max];
const DEEPSEEK_V4: &[Level] = &[Off, Low, High, Max];
const GLM_5_2: &[Level] = &[Off, High, Max];
const GLM_5_3: &[Level] = &[Low, High, Max];
const BAILIAN_FULL: &[Level] = &[Off, Minimal, Low, Medium, High, Xhigh, Max];
const BAILIAN_NO_MAX: &[Level] = &[Off, Minimal, Low, Medium, High, Xhigh];
const BAILIAN_DEEPSEEK: &[Level] = &[Low, Medium, High, Xhigh, Max];
const MINIMAX_M3: &[Level] = &[Off, Medium];
const MINIMAX: &[Level] = &[Medium];
const STEPFUN: &[Level] = &[Off, Low, Medium, High];
const CLAUDE_5: &[Level] = &[Off, Low, Medium, High, Xhigh, Max];
const CLAUDE_5_NO_OFF: &[Level] = &[Low, Medium, High, Xhigh, Max];
const ONLY_OFF: &[Level] = &[Off];
const ALL_OPENAI: &[Level] = &[Off, Minimal, Low, Medium, High, Xhigh];
const GPT56: &[Level] = &[Low, Medium, High, Xhigh, Max, Ultra];
const GPT56_LUNA: &[Level] = &[Low, Medium, High, Xhigh, Max];
const GPT51: &[Level] = &[Off, Low, Medium, High];
const GPT51_CODEX_MAX: &[Level] = &[Off, Low, Medium, High, Xhigh];
const GPT51_CODEX_MINI: &[Level] = &[Off, Medium, High];
const GROK: &[Level] = &[Low, Medium, High];

/// The models a subscription offers: what its service listed, else the
/// TypeScript core's static table.
pub fn subscription_models(subscription: crate::auth::Subscription) -> Vec<String> {
    if let Some(found) = crate::discover::found(subscription) {
        return found.into_iter().map(|m| m.id).collect();
    }
    static_subscription_models(subscription).iter().map(|m| (*m).to_owned()).collect()
}

fn static_subscription_models(subscription: crate::auth::Subscription) -> &'static [&'static str] {
    match subscription {
        crate::auth::Subscription::ChatGpt => &[
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.2-codex",
            "gpt-5.1-codex-max",
            "gpt-5.1-codex-mini",
        ],
        crate::auth::Subscription::Grok => &["grok-4.5", "grok-composer-2.5-fast"],
    }
}

/// Providers and models that must not call tools in parallel.
pub fn serial_tool_calls(provider: &str, model: &str) -> bool {
    (provider == "fireworks" && (model.contains("kimi") || model.contains("k2p6") || model == "k2.6")) || provider == "doubao"
}

/// Whether a ChatGPT model takes Codex's "responses lite" request shape.
pub fn codex_lite(model: &str) -> bool {
    if let Some(found) = crate::discover::lookup("openai", model) {
        return found.lite;
    }
    matches!(model, "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna")
}

/// A model's window and reasoning ladder; `None` where Bubble does not know.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub context_window: Option<u64>,
    pub levels: Cow<'static, [Level]>,
}

fn known(window: u64, levels: &'static [Level]) -> ModelInfo {
    ModelInfo { context_window: Some(window), levels: Cow::Borrowed(levels) }
}

fn is_zhipu(provider: &str) -> bool {
    matches!(provider, "zhipuai" | "zhipuai-coding-plan" | "zai" | "zai-coding-plan")
}

fn is_moonshot(provider: &str) -> bool {
    matches!(provider, "moonshot-cn" | "moonshot-intl" | "kimi-for-coding")
}

pub fn info(provider: &str, model: &str) -> ModelInfo {
    if let Some(found) = crate::discover::lookup(provider, model) {
        let levels = if found.levels.is_empty() { vec![Off] } else { found.levels };
        return ModelInfo { context_window: found.context_window, levels: Cow::Owned(levels) };
    }
    let provider = provider.to_ascii_lowercase();
    let provider = provider.as_str();
    match (provider, model) {
        ("kimi-for-coding", "k3") => known(1_048_576, K3),
        ("kimi-for-coding", "k3-256k") => known(262_144, K3_EFFORT_ONLY),
        ("kimi-for-coding", "k3.1-pro" | "k3.1-pro-highspeed") => known(1_048_576, K3_EFFORT_ONLY),
        ("kimi-for-coding", "kimi-for-coding" | "kimi-for-coding-highspeed" | "kimi-k2.7-code" | "kimi-k2.7-code-highspeed") => {
            known(262_144, THINKING_ONLY)
        }
        ("moonshot-cn" | "moonshot-intl" | "alibaba", "kimi-k3") => known(1_048_576, K3),
        ("moonshot-cn" | "moonshot-intl" | "alibaba", "kimi-k2.7-code") => known(262_144, THINKING_ONLY),
        ("moonshot-cn" | "moonshot-intl" | "alibaba", "kimi-k2.6") => known(256_000, TOGGLE),
        ("alibaba", "kimi/kimi-k3" | "kimi/kimi-k3.1-pro") => known(1_048_576, KIMI_PASSTHROUGH_K3),
        ("alibaba", "kimi/kimi-k2.6") => known(256_000, TOGGLE),
        ("alibaba", "qwen3.6-plus" | "qwen3.7-max") => known(1_048_576, ONLY_OFF),
        ("deepseek", "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" | "deepseek-v4-pro") => {
            known(1_048_576, DEEPSEEK_V4)
        }
        (p, "glm-5.2") if is_zhipu(p) => known(1_000_000, GLM_5_2),
        (p, "glm-5.3" | "glm-5.3-flash") if is_zhipu(p) => known(1_000_000, GLM_5_3),
        (p, "glm-5.1") if is_zhipu(p) => known(200_000, TOGGLE),
        ("bailian-token-plan", "qwen3.8-max" | "qwen3.8-max-preview" | "glm-5.2") => known(1_048_576, BAILIAN_FULL),
        ("bailian-token-plan", "qwen3.7-max" | "qwen3.7-plus" | "qwen3.6-flash") => known(1_048_576, BAILIAN_NO_MAX),
        ("bailian-token-plan", "deepseek-v4-pro" | "deepseek-v4-flash-0731") => known(1_048_576, BAILIAN_DEEPSEEK),
        ("minimax" | "minimax-anthropic" | "minimax-openai", "MiniMax-M3") => known(1_000_000, MINIMAX_M3),
        ("minimax" | "minimax-anthropic" | "minimax-openai", m) if m.starts_with("MiniMax-M2") => {
            ModelInfo { context_window: None, levels: Cow::Borrowed(MINIMAX) }
        }
        ("stepfun", _) => ModelInfo { context_window: None, levels: Cow::Borrowed(STEPFUN) },
        ("openai", "gpt-6-astra" | "gpt-5.6-sol" | "gpt-5.6-terra") => known(372_000, GPT56),
        ("openai", "gpt-5.6-luna") => known(372_000, GPT56_LUNA),
        ("openai", "gpt-5.5" | "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.3-codex" | "gpt-5.3-codex-spark" | "gpt-5.2-codex" | "gpt-5.2") => {
            known(272_000, ALL_OPENAI)
        }
        ("openai", "gpt-5.1-codex-max") => known(272_000, GPT51_CODEX_MAX),
        ("openai", "gpt-5.1-codex-mini") => known(272_000, GPT51_CODEX_MINI),
        ("openai", "gpt-5.1") => known(272_000, GPT51),
        ("grok", "grok-4.5") => known(500_000, GROK),
        ("grok", m) if ["composer", "fast", "mini", "flash"].iter().any(|w| m.contains(w)) => known(200_000, ONLY_OFF),
        ("grok", _) => known(500_000, GROK),
        ("anthropic", m) if m.starts_with("claude-fable-5") || m.starts_with("claude-opus-5-5") => known(200_000, CLAUDE_5_NO_OFF),
        ("anthropic", m) if m.starts_with("claude-opus-5") || m.starts_with("claude-sonnet-5") => known(200_000, CLAUDE_5),
        ("anthropic", m) if m.starts_with("claude-") => known(200_000, ONLY_OFF),
        _ => ModelInfo { context_window: None, levels: Cow::Borrowed(ONLY_OFF) },
    }
}

/// The level a model runs at for a requested one: the highest it offers at
/// or below the request, else its lowest.
pub fn effective_level(provider: &str, model: &str, requested: Level) -> Level {
    let levels = info(provider, model).levels;
    levels
        .iter()
        .copied()
        .filter(|level| *level <= requested)
        .max()
        .or_else(|| levels.first().copied())
        .unwrap_or(Off)
}

/// How an assistant message carries its reasoning back to an OpenAI-style
/// provider (`reasoningContentEcho`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    Never,
    /// `reasoning_content` on messages with tool calls.
    ToolCalls,
    /// `reasoning_content` on every assistant message, even empty.
    Always,
    /// MiniMax's `reasoning_details`.
    Minimax,
}

/// What an OpenAI Chat Completions request adds for reasoning.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenAiReasoning {
    pub fields: Map<String, Value>,
    pub omit_temperature: bool,
    pub echo: Echo,
}

fn fields(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

pub fn openai_reasoning(provider: &str, model: &str, requested: Level) -> OpenAiReasoning {
    let provider = provider.to_ascii_lowercase();
    let level = effective_level(&provider, model, requested);
    let off = level == Off;
    let effort = level.as_str();
    let mut out = OpenAiReasoning { fields: Map::new(), omit_temperature: false, echo: Echo::ToolCalls };
    let thinking = |enabled: bool| json!({ "thinking": { "type": if enabled { "enabled" } else { "disabled" } } });

    if is_moonshot(&provider) {
        out.omit_temperature = true;
        const K3_FAMILY: &[&str] =
            &["k3", "k3-256k", "kimi-k3", "k3.1-pro", "k3.1-pro-highspeed", "kimi-k3.1-pro", "kimi-k3.1-pro-highspeed"];
        const K27_FAMILY: &[&str] =
            &["kimi-k2.7-code", "kimi-k2.7-code-highspeed", "kimi-for-coding", "kimi-for-coding-highspeed"];
        if K3_FAMILY.contains(&model) {
            out.fields = if off { fields(thinking(false)) } else { fields(json!({ "reasoning_effort": effort })) };
        } else if K27_FAMILY.contains(&model) {
            out.fields = fields(thinking(true));
        } else if matches!(model, "kimi-k2.5" | "kimi-k2.6") {
            out.fields = fields(thinking(!off));
        }
        return out;
    }
    match provider.as_str() {
        "deepseek" if matches!(model, "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" | "deepseek-v4-pro") => {
            if off {
                out.fields = fields(thinking(false));
                out.echo = Echo::Never;
            } else {
                out.fields = fields(json!({ "thinking": { "type": "enabled" }, "reasoning_effort": effort }));
                out.echo = Echo::Always;
            }
        }
        p if is_zhipu(p) => {
            if matches!(model, "glm-5.2" | "glm-5.3" | "glm-5.3-flash") {
                out.fields = if off && model == "glm-5.2" {
                    fields(thinking(false))
                } else {
                    fields(json!({ "thinking": { "type": "enabled", "clear_thinking": false }, "reasoning_effort": effort }))
                };
            } else if !off {
                out.fields = fields(json!({ "thinking": { "type": "enabled", "clear_thinking": false } }));
            }
        }
        "minimax" | "minimax-openai" => {
            out.fields = fields(json!({ "reasoning_split": true }));
            if model == "MiniMax-M3" {
                out.fields.insert("thinking".into(), json!({ "type": if off { "disabled" } else { "adaptive" } }));
            }
            out.echo = Echo::Minimax;
        }
        "bailian-token-plan" => {
            out.fields = fields(json!({ "reasoning_effort": if off { "none" } else { effort } }));
        }
        "alibaba" if model.to_ascii_lowercase().strip_prefix("kimi/").unwrap_or(model).starts_with("kimi-") => {
            out.omit_temperature = true;
            match model {
                "kimi/kimi-k3" | "kimi/kimi-k3.1-pro" | "kimi/kimi-k3.1-pro-highspeed" => {
                    out.fields = fields(json!({ "reasoning_effort": effort }));
                }
                "kimi/kimi-k2.6" => out.fields = fields(json!({ "enable_thinking": !off })),
                "kimi-k3" | "kimi-k2.7-code" | "kimi-k2.6" => {
                    out.fields = fields(json!({ "reasoning_effort": if off { "none" } else { effort } }));
                }
                _ => {}
            }
        }
        "stepfun" => {
            if !off {
                out.fields = fields(json!({ "reasoning_effort": effort }));
            }
            out.echo = Echo::Never;
        }
        "stepfun-api" => out.echo = Echo::Never,
        "grok" => {
            if !off {
                out.fields = fields(json!({ "reasoning_effort": effort }));
            }
        }
        "fireworks" if model.contains("kimi") || model.contains("k2p6") || model == "k2.6" => {
            out.fields = fields(json!({ "max_tokens": 32768 }));
            out.echo = Echo::Never;
        }
        _ => {}
    }
    out
}

/// What an Anthropic Messages request adds for reasoning, and its
/// `max_tokens`.
#[derive(Clone, Debug, PartialEq)]
pub struct AnthropicReasoning {
    pub fields: Map<String, Value>,
    pub max_tokens: u32,
}

pub fn anthropic_reasoning(provider: &str, model: &str, base_url: &str, requested: Level) -> AnthropicReasoning {
    let official = base_url.contains("api.anthropic.com");
    let level = effective_level(provider, model, requested);
    let model_lower = model.to_ascii_lowercase();
    let minimax = model_lower.starts_with("minimax");
    let mimo = provider.contains("mimo") || model_lower.contains("mimo");
    if mimo {
        let mut fields = Map::new();
        fields.insert("thinking".into(), json!({ "type": if requested == Off { "disabled" } else { "enabled" } }));
        return AnthropicReasoning { fields, max_tokens: 131_072 };
    }
    let max_tokens = if minimax {
        if model == "MiniMax-M3" { 128_000 } else { 64_000 }
    } else if !official {
        8192
    } else if model_lower.starts_with("claude-opus-5")
        || model_lower.starts_with("claude-fable-5")
        || model_lower.starts_with("claude-sonnet-5")
        || model_lower.starts_with("claude-opus-4-6")
        || model_lower.starts_with("claude-opus-4-7")
    {
        128_000
    } else if model_lower.starts_with("claude-sonnet-4-6") || model_lower.starts_with("claude-haiku-4-5") {
        64_000
    } else {
        8192
    };
    let mut fields = Map::new();
    if official {
        let bound = model_lower.starts_with("claude-opus-5-5") || model_lower.starts_with("claude-fable-5");
        if level != Off && *info(provider, model).levels != *ONLY_OFF {
            let mut thinking = json!({ "type": "adaptive" });
            if bound {
                thinking["display"] = json!("summarized");
                fields.insert("block_binding".into(), json!({ "prefix_mismatch_behavior": "drop_block" }));
            }
            fields.insert("thinking".into(), thinking);
            let effort = match level {
                Minimal | Low => "low",
                Medium => "medium",
                High => "high",
                Xhigh => "xhigh",
                _ => "max",
            };
            fields.insert("output_config".into(), json!({ "effort": effort }));
        } else if level == Off
            && (model_lower.starts_with("claude-sonnet-5") || (model_lower.starts_with("claude-opus-5") && !bound))
        {
            fields.insert("thinking".into(), json!({ "type": "disabled" }));
        }
    }
    AnthropicReasoning { fields, max_tokens }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_clamp_down_to_what_a_model_offers() {
        assert_eq!(effective_level("kimi-for-coding", "k3", High), High);
        assert_eq!(effective_level("kimi-for-coding", "k3", Medium), Low);
        assert_eq!(effective_level("kimi-for-coding", "k3.1-pro", Off), Low, "no off: the lowest it has");
        assert_eq!(effective_level("alibaba", "kimi/kimi-k3", High), Max);
        assert_eq!(effective_level("somewhere", "unknown", High), Off);
    }

    #[test]
    fn kimi_k3_asks_for_effort_or_turns_thinking_off() {
        let high = openai_reasoning("kimi-for-coding", "k3", High);
        assert_eq!(Value::Object(high.fields), json!({ "reasoning_effort": "high" }));
        assert!(high.omit_temperature);
        let off = openai_reasoning("kimi-for-coding", "k3", Off);
        assert_eq!(Value::Object(off.fields), json!({ "thinking": { "type": "disabled" } }));
        let coding = openai_reasoning("kimi-for-coding", "kimi-for-coding", High);
        assert_eq!(Value::Object(coding.fields), json!({ "thinking": { "type": "enabled" } }));
    }

    #[test]
    fn deepseek_echoes_reasoning_only_while_thinking() {
        let on = openai_reasoning("deepseek", "deepseek-v4-pro", High);
        assert_eq!(on.echo, Echo::Always);
        assert_eq!(on.fields["reasoning_effort"], "high");
        assert_eq!(openai_reasoning("deepseek", "deepseek-v4-pro", Off).echo, Echo::Never);
        assert_eq!(openai_reasoning("deepseek", "deepseek-chat", High).echo, Echo::ToolCalls);
    }

    #[test]
    fn glm_keeps_its_thinking_across_turns() {
        let glm = openai_reasoning("zhipuai-coding-plan", "glm-5.3", High);
        assert_eq!(Value::Object(glm.fields), json!({ "thinking": { "type": "enabled", "clear_thinking": false }, "reasoning_effort": "high" }));
    }

    #[test]
    fn alibaba_hosted_kimi_and_qwen_differ() {
        assert_eq!(openai_reasoning("alibaba", "kimi/kimi-k3.1-pro", High).fields["reasoning_effort"], "max");
        assert_eq!(openai_reasoning("alibaba", "kimi-k2.6", Off).fields["reasoning_effort"], "none");
        assert!(openai_reasoning("alibaba", "qwen3.7-max", High).fields.is_empty());
    }

    #[test]
    fn anthropic_thinks_adaptively_on_the_official_api_only() {
        let official = anthropic_reasoning("anthropic", "claude-opus-5-5", "https://api.anthropic.com", High);
        assert_eq!(official.fields["thinking"], json!({ "type": "adaptive", "display": "summarized" }));
        assert_eq!(official.fields["output_config"], json!({ "effort": "high" }));
        assert_eq!(official.max_tokens, 128_000);
        let compatible = anthropic_reasoning("minimax-anthropic", "MiniMax-M3", "https://api.minimaxi.com/anthropic", High);
        assert!(compatible.fields.is_empty());
        assert_eq!(compatible.max_tokens, 128_000);
    }

    #[test]
    fn chatgpt_and_grok_have_their_ladders() {
        assert_eq!(effective_level("openai", "gpt-5.5", Max), Xhigh);
        assert_eq!(effective_level("openai", "gpt-5.6-sol", Off), Low);
        assert!(codex_lite("gpt-5.6-terra") && !codex_lite("gpt-5.5"));
        assert_eq!(openai_reasoning("grok", "grok-4.5", Max).fields["reasoning_effort"], "high");
        assert!(openai_reasoning("grok", "grok-composer-2.5-fast", High).fields.is_empty());
        assert_eq!(info("grok", "grok-5-mini").context_window, Some(200_000));
    }

    #[test]
    fn windows_come_from_the_catalog() {
        assert_eq!(info("kimi-for-coding", "k3").context_window, Some(1_048_576));
        assert_eq!(info("kimi-for-coding", "k3-256k").context_window, Some(262_144));
        assert_eq!(info("nobody", "nothing").context_window, None);
    }
}
