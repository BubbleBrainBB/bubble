//! `web_search` and `web_fetch`, answered by Exa's hosted MCP server
//! (src/tools/web-search.ts, web-fetch.ts, exa-mcp.ts in the TypeScript core).
//! `BUBBLE_WEB_SEARCH_URL` points them elsewhere; `BUBBLE_EXA_API_KEY` adds a
//! key.

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::provider::ToolSpec;
use crate::tools::ToolOutput;

const DEFAULT_URL: &str = "https://mcp.exa.ai/mcp";
const DEFAULT_RESULTS: u64 = 8;
const TIMEOUT: Duration = Duration::from_secs(120);

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web using a remote search service and return current, structured results.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Web search query" },
                    "numResults": { "type": "number", "description": "Number of search results to return (default: 8)" },
                    "livecrawl": { "type": "string", "description": "Live crawl mode", "enum": ["fallback", "preferred"] },
                    "type": { "type": "string", "description": "Search type", "enum": ["auto", "fast", "deep"] },
                    "contextMaxCharacters": { "type": "number", "description": "Maximum number of characters to return in the result context" },
                },
                "required": ["query"],
            }),
        },
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch and extract the contents of a specific URL using a remote web crawling service.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The URL to fetch and extract" },
                    "query": { "type": "string", "description": "Optional topic to focus the extracted summary/highlights on" },
                    "maxCharacters": { "type": "number", "description": "Maximum number of characters to include in the extracted context" },
                    "livecrawl": { "type": "string", "description": "Live crawl mode", "enum": ["never", "fallback", "preferred", "always"] },
                    "livecrawlTimeout": { "type": "number", "description": "Maximum live crawl time in milliseconds" },
                    "subpages": { "type": "number", "description": "Optional number of linked subpages to fetch as well" },
                    "subpageTarget": { "type": "string", "description": "Optional guidance for which subpages matter most" },
                },
                "required": ["url"],
            }),
        },
    ]
}

/// The server's URL with both tools switched on, and the key if there is one.
pub fn endpoint() -> String {
    let base = std::env::var("BUBBLE_WEB_SEARCH_URL").ok().map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    let mut url = reqwest::Url::parse(base.as_deref().unwrap_or(DEFAULT_URL))
        .unwrap_or_else(|_| reqwest::Url::parse(DEFAULT_URL).unwrap());
    let mut tools: Vec<String> = Vec::new();
    let mut others: Vec<(String, String)> = Vec::new();
    for (key, value) in url.query_pairs() {
        if key == "tools" {
            tools.extend(value.split(',').map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()));
        } else {
            others.push((key.into_owned(), value.into_owned()));
        }
    }
    if tools.is_empty() {
        tools = vec!["web_search_exa".into(), "crawling_exa".into()];
    }
    for needed in ["web_search_exa", "crawling_exa"] {
        if !tools.iter().any(|t| t == needed) {
            tools.push(needed.into());
        }
    }
    if let Some(key) = std::env::var("BUBBLE_EXA_API_KEY").ok().map(|k| k.trim().to_owned()).filter(|k| !k.is_empty()) {
        if !others.iter().any(|(k, _)| k == "exaApiKey") {
            others.push(("exaApiKey".into(), key));
        }
    }
    {
        let mut query = url.query_pairs_mut();
        query.clear();
        for (key, value) in &others {
            query.append_pair(key, value);
        }
        query.append_pair("tools", &tools.join(","));
    }
    url.to_string()
}

/// A number as given (integers stay integers).
fn number(value: &Value) -> Option<Value> {
    value.is_number().then(|| value.clone())
}

fn search_args(args: &Value) -> Result<Value, String> {
    let query = args["query"].as_str().unwrap_or_default().trim();
    if query.is_empty() {
        return Err("Error: query is required".into());
    }
    let mut out = Map::new();
    out.insert("query".into(), json!(query));
    out.insert("type".into(), json!(args["type"].as_str().unwrap_or("auto")));
    out.insert("numResults".into(), number(&args["numResults"]).unwrap_or(json!(DEFAULT_RESULTS)));
    out.insert("livecrawl".into(), json!(args["livecrawl"].as_str().unwrap_or("fallback")));
    if let Some(n) = number(&args["contextMaxCharacters"]) {
        out.insert("contextMaxCharacters".into(), n);
    }
    Ok(Value::Object(out))
}

fn fetch_args(args: &Value) -> Result<Value, String> {
    let url = args["url"].as_str().unwrap_or_default().trim();
    if url.is_empty() {
        return Err("Error: url is required".into());
    }
    let query = args["query"].as_str().map(str::trim).filter(|q| !q.is_empty());
    let mut out = Map::new();
    out.insert("urls".into(), json!([url]));
    out.insert("livecrawl".into(), json!(args["livecrawl"].as_str().unwrap_or("fallback")));
    if let Some(n) = number(&args["livecrawlTimeout"]) {
        out.insert("livecrawlTimeout".into(), n);
    }
    if let Some(n) = number(&args["subpages"]) {
        out.insert("subpages".into(), n);
    }
    if let Some(target) = args["subpageTarget"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
        out.insert("subpageTarget".into(), json!(target));
    }
    out.insert("text".into(), json!(true));
    out.insert(
        "context".into(),
        number(&args["maxCharacters"]).map(|n| json!({ "maxCharacters": n })).unwrap_or(json!(true)),
    );
    out.insert("summary".into(), query.map(|q| json!({ "query": q })).unwrap_or(json!(true)));
    if let Some(query) = query {
        out.insert("highlights".into(), json!({ "query": query, "numSentences": 3, "highlightsPerUrl": 5 }));
    }
    Ok(Value::Object(out))
}

/// The first text block of a JSON-RPC result, from events or plain JSON.
fn result_text(body: &str) -> Option<String> {
    let text_of = |value: &Value| {
        value["result"]["content"]
            .as_array()?
            .iter()
            .find(|item| item["type"] == "text")
            .and_then(|item| item["text"].as_str())
            .map(str::to_owned)
    };
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if let Some(text) = serde_json::from_str::<Value>(data).ok().as_ref().and_then(text_of) {
                return Some(text);
            }
        }
    }
    serde_json::from_str::<Value>(body).ok().as_ref().and_then(text_of)
}

/// Runs `web_search` or `web_fetch`.
pub async fn execute(client: &reqwest::Client, name: &str, args: &Value) -> ToolOutput {
    let (tool, arguments) = match name {
        "web_search" => ("web_search_exa", search_args(args)),
        _ => ("crawling_exa", fetch_args(args)),
    };
    let arguments = match arguments {
        Ok(arguments) => arguments,
        Err(message) => return ToolOutput { content: message, is_error: true },
    };
    let request = client
        .post(endpoint())
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("user-agent", "bubble")
        .timeout(TIMEOUT)
        .body(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": arguments } }).to_string());
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return ToolOutput { content: format!("Error: {error}"), is_error: true },
    };
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return ToolOutput {
            content: format!(
                "Error: Remote MCP request failed with status {}{}",
                status.as_u16(),
                if body.is_empty() { String::new() } else { format!(": {body}") }
            ),
            is_error: true,
        };
    }
    ToolOutput { content: result_text(&body).unwrap_or_else(|| "No results found.".into()), is_error: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_follow_the_typescript_tools() {
        assert_eq!(
            search_args(&json!({ "query": " rust " })).unwrap(),
            json!({ "query": "rust", "type": "auto", "numResults": 8, "livecrawl": "fallback" })
        );
        assert!(search_args(&json!({})).is_err());
        let fetch = fetch_args(&json!({ "url": "https://x", "query": "api", "maxCharacters": 500 })).unwrap();
        assert_eq!(fetch["urls"], json!(["https://x"]));
        assert_eq!(fetch["context"], json!({ "maxCharacters": 500 }));
        assert_eq!(fetch["summary"], json!({ "query": "api" }));
        assert_eq!(fetch["highlights"]["highlightsPerUrl"], 5);
        assert_eq!(fetch_args(&json!({ "url": "https://x" })).unwrap()["summary"], true);
    }

    #[test]
    fn results_come_from_events_or_json() {
        let event = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"found\"}]}}\n";
        assert_eq!(result_text(event).as_deref(), Some("found"));
        assert_eq!(result_text("{\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"plain\"}]}}").as_deref(), Some("plain"));
        assert_eq!(result_text("{}"), None);
    }

    #[test]
    fn the_endpoint_enables_both_tools() {
        let url = endpoint();
        assert!(url.starts_with("https://mcp.exa.ai/mcp?"));
        assert!(url.contains("tools=web_search_exa%2Ccrawling_exa"));
    }
}
