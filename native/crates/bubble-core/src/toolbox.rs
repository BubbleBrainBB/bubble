//! The tools beyond the project folder's: skills, web search and fetch, and
//! MCP servers' tools. MCP tools are deferred, as in the TypeScript core:
//! the system prompt names them and `tool_search` loads a schema when one is
//! needed, so a dozen servers do not fill the context.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::config::{Config, bubble_home};
use crate::mcp::{self, Hub, Server};
use crate::provider::{ToolCall, ToolSpec};
use crate::skills::{self, Skills};
use crate::tools::{Permission, ToolOutput};
use crate::web;

pub struct Toolbox {
    pub skills: Skills,
    pub servers: Vec<Arc<Server>>,
    pub web: bool,
    /// The memory tool and the memory summary in the prompt.
    pub memory: bool,
    catalog_chars: usize,
    http: reqwest::Client,
}

/// One MCP tool on offer.
struct Deferred {
    spec: ToolSpec,
    server: Arc<Server>,
    tool: String,
}

impl Toolbox {
    /// Everything for a project (or none): its skills, and its MCP servers,
    /// connected as far as they will.
    pub async fn load(hub: &Hub, http: &reqwest::Client, config: &Config, project: Option<&Path>) -> Toolbox {
        let home = bubble_home();
        let skills = Skills::discover(&Skills::roots(&home, project, &config.skill_paths), &config.skills.disabled);
        let (configured, diagnostics) = mcp::load_config(&home, project);
        for diagnostic in diagnostics {
            log::warn!("mcp: {diagnostic}");
        }
        let servers = hub.connect(configured, http).await;
        Toolbox {
            skills,
            servers,
            web: true,
            memory: true,
            catalog_chars: config.skills.catalog_chars.unwrap_or(skills::CATALOG_CHARS),
            http: http.clone(),
        }
    }

    /// No skills, servers or web: for tests and hosts that want none.
    pub fn empty(http: &reqwest::Client) -> Toolbox {
        Toolbox { skills: Skills::default(), servers: Vec::new(), web: false, memory: false, catalog_chars: 0, http: http.clone() }
    }

    fn deferred(&self) -> Vec<Deferred> {
        self.servers
            .iter()
            .flat_map(|server| {
                server.specs().into_iter().map(|(spec, tool)| Deferred { spec, server: server.clone(), tool })
            })
            .collect()
    }

    /// What to add to the system prompt: the skills, and the deferred tools.
    pub fn prompt(&self) -> Option<String> {
        let mut sections = Vec::new();
        let deferred = self.deferred();
        if !deferred.is_empty() {
            let names: Vec<String> = deferred.iter().map(|d| d.spec.name.clone()).collect();
            sections.push(format!(
                "## Deferred tools\nThe following deferred tools are available via tool_search. Their schemas are NOT loaded — calling them directly will fail. Use tool_search with query \"select:<name>[,<name>...]\" to load tool schemas before calling them:\n\n{}",
                names.join("\n")
            ));
        }
        if self.memory {
            if let Some(memory) = crate::memory::prompt(&crate::memory::root()) {
                sections.push(memory);
            }
        }
        if let Some(catalog) = self.skills.catalog(self.catalog_chars) {
            sections.push(catalog);
        }
        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    /// The tools to offer now: skills, web, `tool_search` and the MCP tools
    /// loaded so far.
    pub fn specs(&self, unlocked: &HashSet<String>) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        if self.skills.visible().next().is_some() {
            specs.extend(Skills::specs());
        }
        if self.web {
            specs.extend(web::specs());
        }
        if self.memory {
            specs.push(crate::memory::spec());
        }
        let deferred = self.deferred();
        if !deferred.is_empty() {
            specs.push(tool_search_spec());
            specs.extend(deferred.into_iter().filter(|d| unlocked.contains(&d.spec.name)).map(|d| d.spec));
        }
        specs
    }

    /// Whether a call is one of these tools.
    pub fn owns(&self, name: &str) -> bool {
        match name {
            "skill" | "skill_search" => self.skills.visible().next().is_some(),
            "web_search" | "web_fetch" => self.web,
            "memory" => self.memory,
            "tool_search" => !self.servers.is_empty(),
            other => mcp::parse_tool_name(other).is_some() && self.deferred().iter().any(|d| d.spec.name == other),
        }
    }

    /// MCP servers can do anything (open PRs, post, write databases), and
    /// their read-only hints are their own claims, so each call asks. A tool
    /// not loaded yet fails without asking.
    pub fn permission(&self, call: &ToolCall, unlocked: &Mutex<HashSet<String>>) -> Option<Permission> {
        let (server, tool) = mcp::parse_tool_name(&call.name)?;
        if !unlocked.lock().map(|u| u.contains(&call.name)).unwrap_or(false) {
            return None;
        }
        let detail = serde_json::to_string_pretty(&call.arguments_json()).unwrap_or_default();
        Some(Permission {
            grant: format!("mcp:{}", call.name),
            summary: format!("Use {tool} from the {server} MCP server"),
            detail,
            rememberable: true,
        })
    }

    pub async fn execute(&self, call: &ToolCall, unlocked: &Mutex<HashSet<String>>) -> ToolOutput {
        let args = call.arguments_json();
        match call.name.as_str() {
            "skill" => self.skills.load_tool(&args),
            "skill_search" => self.skills.search_tool(&args),
            "web_search" | "web_fetch" => web::execute(&self.http, &call.name, &args).await,
            "tool_search" => self.tool_search(&args, unlocked),
            "memory" => crate::memory::tool(&crate::memory::root(), &args),
            name => {
                let is_unlocked = unlocked.lock().map(|u| u.contains(name)).unwrap_or(false);
                if !is_unlocked {
                    return ToolOutput {
                        content: format!(
                            "Error: Tool \"{name}\" is a deferred tool; its schema is not yet loaded. Call tool_search first with query \"select:{name}\" to load its schema, then retry."
                        ),
                        is_error: true,
                    };
                }
                match self.deferred().into_iter().find(|d| d.spec.name == name) {
                    Some(d) => d.server.call(&d.tool, args).await,
                    None => ToolOutput { content: format!("Error: there is no tool called {name}."), is_error: true },
                }
            }
        }
    }

    fn tool_search(&self, args: &Value, unlocked: &Mutex<HashSet<String>>) -> ToolOutput {
        let query = args["query"].as_str().unwrap_or_default();
        let max = args["max_results"].as_f64().filter(|n| *n > 0.0).map(|n| (n as usize).min(25)).unwrap_or(5);
        let deferred: Vec<ToolSpec> = self.deferred().into_iter().map(|d| d.spec).collect();
        if deferred.is_empty() {
            return ToolOutput { content: "No deferred tools are registered in this session.".into(), is_error: false };
        }
        let matches: Vec<ToolSpec> = if let Some(list) = query.strip_prefix("select:") {
            let names: HashSet<&str> = list.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
            let found: Vec<ToolSpec> = deferred.iter().filter(|t| names.contains(t.name.as_str())).cloned().collect();
            if found.is_empty() {
                let known: Vec<&str> = deferred.iter().map(|t| t.name.as_str()).collect();
                return ToolOutput {
                    content: format!("No deferred tool matched select list. Known deferred tools: {}", known.join(", ")),
                    is_error: true,
                };
            }
            found
        } else {
            let ranked = rank(&deferred, query);
            if ranked.is_empty() {
                return ToolOutput {
                    content: format!(
                        "No deferred tools matched \"{query}\". Use query \"select:<name>\" to fetch by exact name, or try different keywords."
                    ),
                    is_error: false,
                };
            }
            ranked.into_iter().take(max).collect()
        };
        if let Ok(mut unlocked) = unlocked.lock() {
            unlocked.extend(matches.iter().map(|t| t.name.clone()));
        }
        let mut lines = vec!["<functions>".to_owned()];
        for tool in &matches {
            let schema = json!({ "description": tool.description, "name": tool.name, "parameters": tool.parameters });
            lines.push(format!("<function>{schema}</function>"));
        }
        lines.push("</functions>".into());
        lines.push(String::new());
        lines.push(format!(
            "Loaded {} tool{}. They are now available and callable on the next turn.",
            matches.len(),
            if matches.len() == 1 { "" } else { "s" }
        ));
        ToolOutput { content: lines.join("\n"), is_error: false }
    }
}

fn tool_search_spec() -> ToolSpec {
    ToolSpec {
        name: "tool_search".into(),
        description: "Fetches full schema definitions for deferred tools so they can be called. Deferred tools are named in the system prompt; their parameters are unknown until loaded. Use this tool with query \"select:<name>[,<name>...]\" to load specific tools, or with free-text keywords to search for relevant tools.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Query to find deferred tools. Use \"select:<name>,<name>\" for direct selection, or keywords to search." },
                "max_results": { "type": "number", "description": "Maximum number of matches to return (default 5)." },
            },
            "required": ["query"],
        }),
    }
}

/// Free-text search: name hits count 3, description hits 1; `+term` must
/// match.
fn rank(tools: &[ToolSpec], query: &str) -> Vec<ToolSpec> {
    let lower = query.to_lowercase();
    let terms: Vec<&str> = lower.split_whitespace().collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let required: Vec<&str> = terms.iter().filter_map(|t| t.strip_prefix('+').filter(|t| !t.is_empty())).collect();
    let optional: Vec<&str> = terms.iter().filter(|t| !(t.starts_with('+') && t.len() > 1)).copied().collect();
    let mut scored: Vec<(usize, &ToolSpec)> = Vec::new();
    for tool in tools {
        let name = tool.name.to_lowercase();
        let description = tool.description.to_lowercase();
        let haystack = format!("{name} {description}");
        if !required.iter().all(|t| haystack.contains(t)) {
            continue;
        }
        let mut score = 0;
        for term in &optional {
            if name.contains(term) {
                score += 3;
            }
            if description.contains(term) {
                score += 1;
            }
        }
        if !required.is_empty() && optional.is_empty() {
            score = required.iter().map(|t| if name.contains(t) { 3 } else { 1 }).sum();
        }
        if score > 0 {
            scored.push((score, tool));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, t)| t.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec { name: name.into(), description: description.into(), parameters: json!({}) }
    }

    #[test]
    fn ranking_weighs_names_and_requires_plus_terms() {
        let tools = [
            spec("mcp__arxiv__search_papers", "[MCP:arxiv] Search papers"),
            spec("mcp__arxiv__download_paper", "[MCP:arxiv] Download one paper"),
            spec("mcp__slack__send", "[MCP:slack] Send a message"),
        ];
        let names = |found: Vec<ToolSpec>| found.into_iter().map(|t| t.name).collect::<Vec<_>>();
        assert_eq!(names(rank(&tools, "search paper")), ["mcp__arxiv__search_papers", "mcp__arxiv__download_paper"]);
        assert_eq!(names(rank(&tools, "+slack")), ["mcp__slack__send"]);
        assert!(rank(&tools, "").is_empty());
    }
}
