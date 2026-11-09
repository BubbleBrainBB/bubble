//! Google's Gemini API, natively (`POST {baseURL}/models/{model}:streamGenerateContent?alt=sse`,
//! the TypeScript CLI's `ai-sdk` protocol): the request, and the stream's
//! thoughts, text, function calls and usage. The model's parts come back
//! with thought signatures, which go back to the same model on later turns
//! (Gemini 3 requires them for function calls).

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::config::ResolvedModel;
use crate::model::Level;
use crate::provider::{ChatMessage, ChatRequest};

pub fn url(base_url: &str, model: &str) -> String {
    format!("{}/models/{model}:streamGenerateContent?alt=sse", base_url.trim_end_matches('/'))
}

/// The JSON-schema keywords Gemini's function declarations accept.
const SCHEMA_KEYS: &[&str] =
    &["type", "format", "description", "nullable", "enum", "properties", "required", "items", "minItems", "maxItems", "minimum", "maximum", "anyOf", "propertyOrdering"];

/// A tool's JSON schema as Gemini takes it: unknown keywords dropped,
/// `["string", "null"]` types made nullable.
pub fn schema(value: &Value) -> Value {
    let Some(object) = value.as_object() else { return value.clone() };
    let mut out = Map::new();
    for (key, item) in object {
        if !SCHEMA_KEYS.contains(&key.as_str()) {
            continue;
        }
        let item = match key.as_str() {
            "type" => match item {
                Value::Array(types) => {
                    let real: Vec<&Value> = types.iter().filter(|t| *t != "null").collect();
                    if real.len() < types.len() {
                        out.insert("nullable".into(), json!(true));
                    }
                    real.first().map(|t| (*t).clone()).unwrap_or(json!("string"))
                }
                other => other.clone(),
            },
            "properties" => Value::Object(
                item.as_object().map(|p| p.iter().map(|(k, v)| (k.clone(), schema(v))).collect()).unwrap_or_default(),
            ),
            "items" => schema(item),
            "anyOf" => Value::Array(item.as_array().map(|a| a.iter().map(schema).collect()).unwrap_or_default()),
            _ => item.clone(),
        };
        out.insert(key.clone(), item);
    }
    Value::Object(out)
}

/// Thinking settings: levels on Gemini 3, token budgets on 2.5.
fn thinking(model: &str, level: Level) -> Option<Value> {
    if model.contains("gemini-3") || model.contains("gemini-4") {
        let level = if level <= Level::Low { "low" } else { "high" };
        return Some(json!({ "thinkingLevel": level, "includeThoughts": true }));
    }
    if model.contains("2.5") {
        let budget = match level {
            Level::Off if model.contains("flash") => 0,
            Level::Off | Level::Minimal => 512,
            Level::Low => 1024,
            Level::Medium => 8192,
            Level::High => 24576,
            _ => -1,
        };
        return Some(json!({ "thinkingBudget": budget, "includeThoughts": budget != 0 }));
    }
    None
}

pub fn body(model: &ResolvedModel, request: &ChatRequest) -> Value {
    // Tool results name their function; Gemini wants the name back.
    let mut names: HashMap<String, String> = HashMap::new();
    let mut contents = Vec::new();
    for message in &request.messages {
        match message {
            ChatMessage::User { text, images } => {
                let mut parts = vec![json!({ "text": text })];
                parts.extend(images.iter().map(|i| json!({ "inlineData": { "mimeType": i.media_type, "data": i.data } })));
                contents.push(json!({ "role": "user", "parts": parts }));
            }
            ChatMessage::Assistant { text, tool_calls, state, .. } => {
                for call in tool_calls {
                    names.insert(call.id.clone(), call.name.clone());
                }
                let kept = crate::provider::echoed(state, "gemini", model);
                let parts: Vec<Value> = if !kept.is_empty() {
                    kept.to_vec()
                } else {
                    let mut parts = Vec::new();
                    if !text.is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                    parts.extend(tool_calls.iter().map(|c| json!({ "functionCall": { "name": c.name, "args": c.arguments_json() } })));
                    parts
                };
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
            ChatMessage::Tool { results } => {
                let parts: Vec<Value> = results
                    .iter()
                    .map(|r| {
                        let name = names.get(&r.call_id).cloned().unwrap_or_else(|| r.call_id.clone());
                        let key = if r.is_error { "error" } else { "output" };
                        json!({ "functionResponse": { "name": name, "response": { key: r.content } } })
                    })
                    .collect();
                contents.push(json!({ "role": "user", "parts": parts }));
            }
        }
    }
    let mut body = json!({
        "contents": contents,
        "generationConfig": { "maxOutputTokens": request.max_tokens },
    });
    if let Some(system) = request.system.as_deref().filter(|s| !s.trim().is_empty()) {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if !request.tools.is_empty() {
        let declarations: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                let mut declaration = json!({ "name": t.name, "description": t.description });
                let parameters = schema(&t.parameters);
                // An object without properties is refused: no parameters.
                if parameters["properties"].as_object().is_some_and(|p| !p.is_empty()) {
                    declaration["parameters"] = parameters;
                }
                declaration
            })
            .collect();
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    if let Some(thinking) = thinking(&model.model, request.thinking) {
        body["generationConfig"]["thinkingConfig"] = thinking;
    }
    body
}

/// Adds one streamed part to the kept parts: text runs of the same kind
/// are joined until a signature closes them.
pub fn keep_part(parts: &mut Vec<Value>, part: &Value) {
    let thought = part["thought"] == true;
    if let (Some(text), Some(last)) = (part["text"].as_str(), parts.last_mut())
        && last["text"].is_string()
        && (last["thought"] == true) == thought
        && last.get("thoughtSignature").is_none()
    {
        let joined = format!("{}{text}", last["text"].as_str().unwrap_or_default());
        last["text"] = json!(joined);
        if let Some(signature) = part.get("thoughtSignature") {
            last["thoughtSignature"] = signature.clone();
        }
        return;
    }
    parts.push(part.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ToolCall, ToolResult, ToolSpec};

    #[test]
    fn schemas_lose_what_gemini_refuses() {
        let tool = json!({
            "$schema": "x", "type": "object", "additionalProperties": false,
            "properties": { "path": { "type": ["string", "null"], "description": "Where", "default": "." }, "n": { "type": "integer" } },
            "required": ["path"]
        });
        assert_eq!(
            schema(&tool),
            json!({ "type": "object", "properties": { "path": { "type": "string", "nullable": true, "description": "Where" }, "n": { "type": "integer" } }, "required": ["path"] })
        );
    }

    #[test]
    fn requests_carry_images_calls_results_and_thinking() {
        let config = crate::config::Config::parse(
            r#"{"providers":[{"id":"google","baseURL":"https://generativelanguage.googleapis.com/v1beta","apiKey":"k","protocol":"ai-sdk"}]}"#,
        )
        .unwrap();
        let model = config.resolve("google:gemini-3-pro").unwrap();
        assert_eq!(model.protocol, crate::config::Protocol::Gemini);
        let request = ChatRequest {
            system: Some("Be brief.".into()),
            messages: vec![
                ChatMessage::user_with_images("look", vec![crate::provider::Image { media_type: "image/png".into(), data: "AA".into() }]),
                ChatMessage::Assistant {
                    text: String::new(),
                    reasoning: String::new(),
                    tool_calls: vec![ToolCall { id: "call_0".into(), name: "ls".into(), arguments: "{\"path\":\"src\"}".into() }],
                    state: json!({ "kind": "gemini", "provider": "google", "model": "gemini-3-pro",
                                   "items": [{ "functionCall": { "name": "ls", "args": { "path": "src" } }, "thoughtSignature": "sig" }] }),
                },
                ChatMessage::Tool { results: vec![ToolResult { call_id: "call_0".into(), content: "a.rs".into(), is_error: false }] },
            ],
            tools: vec![ToolSpec { name: "ls".into(), description: "List".into(), parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }) }],
            max_tokens: 1000,
            thinking: Level::High,
        };
        let body = body(&model, &request);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be brief.");
        assert_eq!(body["contents"][0]["parts"][1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(body["contents"][1]["parts"][0]["thoughtSignature"], "sig", "the signed part goes back as it came");
        assert_eq!(body["contents"][2]["parts"][0]["functionResponse"], json!({ "name": "ls", "response": { "output": "a.rs" } }));
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["path"]["type"], "string");
        assert_eq!(body["generationConfig"]["thinkingConfig"], json!({ "thinkingLevel": "high", "includeThoughts": true }));
    }

    #[test]
    fn text_parts_join_until_signed() {
        let mut parts = Vec::new();
        keep_part(&mut parts, &json!({ "text": "Thinking ", "thought": true }));
        keep_part(&mut parts, &json!({ "text": "hard", "thought": true, "thoughtSignature": "s1" }));
        keep_part(&mut parts, &json!({ "text": "Answer" }));
        keep_part(&mut parts, &json!({ "text": "." }));
        assert_eq!(parts, [json!({ "text": "Thinking hard", "thought": true, "thoughtSignature": "s1" }), json!({ "text": "Answer." })]);
    }
}
