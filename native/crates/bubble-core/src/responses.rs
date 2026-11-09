//! Request bodies for OpenAI-style Responses APIs: ChatGPT's Codex backend,
//! Volcano Engine Ark and OpenAI-compatible ones (provider-openai-codex.ts,
//! provider-ark-responses.ts in the TypeScript core).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{Dialect, ResolvedModel};
use crate::model::{self, Level};
use crate::provider::{ChatMessage, ChatRequest};

/// The URL a request goes to.
pub fn url(dialect: Dialect, base_url: &str) -> String {
    match dialect {
        Dialect::Codex if base_url.ends_with("/codex/responses") => base_url.to_owned(),
        Dialect::Codex if base_url.ends_with("/codex") => format!("{base_url}/responses"),
        Dialect::Codex => format!("{base_url}/codex/responses"),
        Dialect::Ark | Dialect::OpenAi => format!("{base_url}/responses"),
    }
}

fn user_item(text: &str) -> Value {
    json!({ "role": "user", "content": [{ "type": "input_text", "text": text }] })
}

/// The conversation as input items.
fn input(messages: &[ChatMessage]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message {
            ChatMessage::User { text } => items.push(user_item(text)),
            ChatMessage::Assistant { text, tool_calls, .. } => {
                if !text.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{ "type": "output_text", "text": text, "annotations": [] }],
                    }));
                }
                for call in tool_calls {
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": if call.arguments.trim().is_empty() { "{}" } else { call.arguments.as_str() },
                    }));
                }
            }
            ChatMessage::Tool { results } => {
                for result in results {
                    items.push(json!({ "type": "function_call_output", "call_id": result.call_id, "output": result.content }));
                }
            }
        }
    }
    items
}

fn tools(request: &ChatRequest) -> Vec<Value> {
    request
        .tools
        .iter()
        .map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.parameters }))
        .collect()
}

/// The prompt-cache key of a model in a session.
pub fn cache_key(model: &str, seed: &str) -> String {
    let digest = Sha256::digest(format!("bubble:openai-codex:{model}:{seed}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn body(dialect: Dialect, model: &ResolvedModel, request: &ChatRequest, session_id: &str) -> Value {
    let system = request.system.clone().filter(|s| !s.trim().is_empty());
    let level = model::effective_level(&model.provider.id, &model.model, request.thinking);
    match dialect {
        Dialect::Codex => {
            let lite = model::codex_lite(&model.model);
            let mut body = json!({
                "model": model.model,
                "store": false,
                "stream": true,
                "include": ["reasoning.encrypted_content"],
                "parallel_tool_calls": !lite,
                "text": { "verbosity": "medium" },
                "prompt_cache_key": cache_key(&model.model, session_id),
            });
            let mut items = Vec::new();
            if lite {
                body["instructions"] = json!("");
                items.push(json!({ "type": "additional_tools", "role": "developer", "tools": tools(request) }));
                items.push(json!({
                    "role": "developer",
                    "content": [{ "type": "input_text", "text": system.clone().unwrap_or_default() }],
                }));
                body["tool_choice"] = json!("auto");
                body["client_metadata"] = json!({
                    "x-codex-installation-id": session_id,
                    "session_id": session_id,
                    "thread_id": session_id,
                    "x-codex-window-id": session_id,
                });
            } else {
                if let Some(system) = &system {
                    body["instructions"] = json!(system);
                }
                if !request.tools.is_empty() {
                    body["tools"] = json!(tools(request));
                    body["tool_choice"] = json!("auto");
                }
            }
            items.extend(input(&request.messages));
            body["input"] = json!(items);
            if level != Level::Off {
                let effort = if level == Level::Ultra { "max" } else { level.as_str() };
                body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
                if lite {
                    body["reasoning"]["context"] = json!("all_turns");
                }
            }
            body
        }
        Dialect::Ark => {
            let mut items = Vec::new();
            if let Some(system) = &system {
                items.push(json!({ "type": "message", "role": "system", "content": [{ "type": "input_text", "text": system }] }));
            }
            items.extend(input(&request.messages));
            let thinking = if matches!(request.thinking, Level::Off | Level::Minimal) { "disabled" } else { "enabled" };
            let mut body = json!({
                "model": model.model,
                "input": items,
                "store": false,
                "stream": true,
                "thinking": { "type": thinking },
            });
            if !request.tools.is_empty() {
                body["tools"] = json!(tools(request));
            }
            body
        }
        Dialect::OpenAi => {
            let mut body = json!({
                "model": model.model,
                "input": input(&request.messages),
                "store": false,
                "stream": true,
            });
            if let Some(system) = &system {
                body["instructions"] = json!(system);
            }
            if !request.tools.is_empty() {
                body["tools"] = json!(tools(request));
                body["tool_choice"] = json!("auto");
            }
            if level != Level::Off {
                let effort = if level >= Level::Max { "xhigh" } else { level.as_str() };
                body["reasoning"] = json!({ "effort": effort, "summary": "auto" });
            }
            body
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolCall, ToolResult, ToolSpec};

    fn resolved(provider: &str, model: &str) -> ResolvedModel {
        ResolvedModel {
            provider: crate::config::ProviderConfig {
                id: provider.into(),
                name: None,
                base_url: None,
                api_key: None,
                enabled: true,
                protocol: None,
                headers: Default::default(),
            },
            model: model.into(),
            protocol: crate::config::Protocol::Responses(Dialect::Codex),
            base_url: crate::config::CODEX_BASE_URL.into(),
            api_key: String::new(),
            subscription: None,
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            system: Some("Be brief.".into()),
            messages: vec![
                ChatMessage::user("list"),
                ChatMessage::Assistant {
                    text: String::new(),
                    reasoning: "r".into(),
                    tool_calls: vec![ToolCall { id: "call_1".into(), name: "ls".into(), arguments: String::new() }],
                },
                ChatMessage::Tool { results: vec![ToolResult { call_id: "call_1".into(), content: "a.rs".into(), is_error: false }] },
            ],
            tools: vec![ToolSpec { name: "ls".into(), description: "List".into(), parameters: json!({"type":"object"}) }],
            max_tokens: 1000,
            thinking: Level::High,
        }
    }

    #[test]
    fn codex_urls_append_once() {
        assert_eq!(url(Dialect::Codex, "https://chatgpt.com/backend-api"), "https://chatgpt.com/backend-api/codex/responses");
        assert_eq!(url(Dialect::Codex, "https://x/codex"), "https://x/codex/responses");
        assert_eq!(url(Dialect::Ark, "https://ark/api/v3"), "https://ark/api/v3/responses");
    }

    #[test]
    fn codex_bodies_carry_instructions_items_and_effort() {
        let body = body(Dialect::Codex, &resolved("openai", "gpt-5.5"), &request(), "s1");
        assert_eq!(body["instructions"], "Be brief.");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"], json!({ "effort": "high", "summary": "auto" }));
        assert_eq!(body["tools"][0]["name"], "ls");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][1]["arguments"], "{}");
        assert_eq!(body["input"][2], json!({ "type": "function_call_output", "call_id": "call_1", "output": "a.rs" }));
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(body["prompt_cache_key"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn lite_models_put_tools_and_instructions_in_the_input() {
        let body = body(Dialect::Codex, &resolved("openai", "gpt-5.6-sol"), &request(), "s1");
        assert_eq!(body["instructions"], "");
        assert!(body.get("tools").is_none());
        assert_eq!(body["input"][0]["type"], "additional_tools");
        assert_eq!(body["input"][1]["role"], "developer");
        assert_eq!(body["reasoning"]["context"], "all_turns");
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn ark_puts_the_system_prompt_inline_and_toggles_thinking() {
        let body = body(Dialect::Ark, &resolved("doubao", "doubao-seed"), &request(), "s1");
        assert_eq!(body["input"][0]["role"], "system");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("instructions").is_none());
    }
}
