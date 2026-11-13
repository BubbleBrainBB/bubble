//! The person's permission rules: what the bots' agent lets go ahead without
//! asking, and what it never allows. They are the agent's own rules, in
//! `settings.json` beside config.json (`permissions.allow` and
//! `permissions.deny`), which it reads at the start of each turn; the app
//! only shows them and changes the two lists, leaving the rest of the file
//! as it is.
//!
//! A rule the person writes here is the beginning of a command ("git
//! status"), kept as the agent writes it (`Bash(git status:*)`). Rules for
//! other tools, written by hand in the file, show as they are.

use std::path::{Path, PathBuf};

use bubble_core::config::bubble_home;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    /// Goes ahead without asking.
    Allow,
    /// Refused, whatever else is set.
    Deny,
}

impl List {
    fn key(self) -> &'static str {
        match self {
            List::Allow => "allow",
            List::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Rules {
    pub fn of(&self, list: List) -> &[String] {
        match list {
            List::Allow => &self.allow,
            List::Deny => &self.deny,
        }
    }
}

/// Where the rules are kept.
pub fn path() -> PathBuf {
    bubble_home().join("settings.json")
}

/// The tools a rule can name, as the agent knows them.
const TOOLS: [&str; 8] = ["Bash", "Read", "Write", "Edit", "Lsp", "WebFetch", "WebSearch", "*"];

/// `Tool` or `Tool(pattern)`, for a tool the agent knows or an MCP server's.
fn is_rule(text: &str) -> bool {
    let (tool, rest) = match text.split_once('(') {
        Some((tool, rest)) => (tool, Some(rest)),
        None => (text, None),
    };
    let known = TOOLS.contains(&tool) || (tool.starts_with("mcp__") && tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    known
        && match rest {
            None => true,
            Some(rest) => rest.ends_with(')') && !rest[..rest.len() - 1].contains(')') && !rest[..rest.len() - 1].trim().is_empty(),
        }
}

/// The rule to keep for what the person wrote: a rule as it is, anything
/// else as the beginning of a command.
pub fn written(text: &str) -> Result<String, String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("Write the command the rule is for.".to_owned());
    }
    if is_rule(&text) {
        return Ok(text);
    }
    if text.contains('(') || text.contains(')') {
        return Err("A rule is the beginning of a command, without parentheses: git status".to_owned());
    }
    let command = text.strip_suffix(":*").unwrap_or(&text).trim_end();
    if command.is_empty() {
        return Err("Write the command the rule is for.".to_owned());
    }
    Ok(format!("Bash({command}:*)"))
}

/// A rule as a person reads it: the command it is for, when it is one for
/// commands that begin so.
pub fn shown(rule: &str) -> String {
    let rule = rule.trim();
    match rule.strip_prefix("Bash(").and_then(|rest| rest.strip_suffix(')')) {
        Some(pattern) => match pattern.trim().strip_suffix(":*") {
            Some(command) if !command.trim().is_empty() => command.trim().to_owned(),
            Some(_) => "Every command".to_owned(),
            None => format!("{}, with nothing after it", pattern.trim()),
        },
        None => rule.to_owned(),
    }
}

fn read(path: &Path) -> Result<Map<String, Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(format!("settings.json could not be read: {error}")),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("settings.json is not a JSON object, so the rules cannot be kept in it.".to_owned()),
        Err(error) => Err(format!("settings.json has a mistake in it ({error}); fix it there before changing rules here.")),
    }
}

fn rules_in(settings: &Map<String, Value>) -> Rules {
    let list = |key: &str| -> Vec<String> {
        settings
            .get("permissions")
            .and_then(|p| p.get(key))
            .and_then(Value::as_array)
            .map(|rules| rules.iter().filter_map(|r| r.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    Rules { allow: list("allow"), deny: list("deny") }
}

/// The rules as the file has them; none when there is no file.
pub fn load(path: &Path) -> Result<Rules, String> {
    read(path).map(|settings| rules_in(&settings))
}

/// Changes one list and writes the file, everything else in it kept.
fn change(path: &Path, list: List, change: impl FnOnce(&mut Vec<Value>)) -> Result<Rules, String> {
    let mut settings = read(path)?;
    let permissions = settings.entry("permissions").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(permissions) = permissions else {
        return Err("\"permissions\" in settings.json is not an object, so the rules cannot be kept in it.".to_owned());
    };
    let rules = permissions.entry(list.key()).or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(rules) = rules else {
        return Err(format!("\"permissions.{}\" in settings.json is not a list.", list.key()));
    };
    change(rules);
    let mut text = serde_json::to_string_pretty(&Value::Object(settings.clone())).map_err(|e| e.to_string())?;
    text.push('\n');
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|e| format!("settings.json could not be written: {e}"))?;
    }
    // Whole or not at all: the agent reads this file too.
    let beside = path.with_extension("json.writing");
    std::fs::write(&beside, text).and_then(|()| std::fs::rename(&beside, path)).map_err(|e| {
        let _ = std::fs::remove_file(&beside);
        format!("settings.json could not be written: {e}")
    })?;
    Ok(rules_in(&settings))
}

/// Adds what the person wrote to a list, once.
pub fn add(path: &Path, list: List, text: &str) -> Result<Rules, String> {
    let rule = written(text)?;
    change(path, list, |rules| {
        if !rules.iter().any(|r| r.as_str() == Some(rule.as_str())) {
            rules.push(Value::String(rule));
        }
    })
}

/// Takes a rule, as the file has it, off a list.
pub fn remove(path: &Path, list: List, rule: &str) -> Result<Rules, String> {
    change(path, list, |rules| rules.retain(|r| r.as_str() != Some(rule)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("bubble-rules-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder.join("settings.json")
    }

    #[test]
    fn a_command_is_kept_as_the_agent_writes_rules() {
        assert_eq!(written("git status").unwrap(), "Bash(git status:*)");
        assert_eq!(written("  npm   test ").unwrap(), "Bash(npm test:*)");
        assert_eq!(written("git push:*").unwrap(), "Bash(git push:*)");
        // A rule written as the agent's stays as it is.
        assert_eq!(written("Bash(git status)").unwrap(), "Bash(git status)");
        assert_eq!(written("Edit").unwrap(), "Edit");
        assert_eq!(written("WebFetch(domain:github.com)").unwrap(), "WebFetch(domain:github.com)");
        assert_eq!(written("mcp__linear").unwrap(), "mcp__linear");
        // A command that happens to be a word is a command.
        assert_eq!(written("ls").unwrap(), "Bash(ls:*)");
        assert!(written("   ").is_err());
        assert!(written("echo $(date)").is_err());
        assert!(written(":*").is_err());
    }

    #[test]
    fn a_rule_reads_as_the_command_it_is_for() {
        assert_eq!(shown("Bash(git status:*)"), "git status");
        assert_eq!(shown("Bash(git status)"), "git status, with nothing after it");
        assert_eq!(shown("Bash(:*)"), "Every command");
        assert_eq!(shown("Edit(src/**)"), "Edit(src/**)");
        assert_eq!(shown("mcp__linear"), "mcp__linear");
    }

    #[test]
    fn the_rest_of_the_file_is_kept() {
        let path = scratch("kept");
        std::fs::write(&path, r#"{"mcpServers":{"linear":{"command":"npx"}},"permissions":{"defaultMode":"default","allow":["Edit"]},"hooks":{}}"#).unwrap();
        let rules = add(&path, List::Allow, "git status").unwrap();
        assert_eq!(rules.allow, ["Edit", "Bash(git status:*)"]);
        // Once: the same rule again changes nothing.
        assert_eq!(add(&path, List::Allow, "git  status").unwrap().allow.len(), 2);
        let rules = add(&path, List::Deny, "rm -rf").unwrap();
        assert_eq!(rules.deny, ["Bash(rm -rf:*)"]);
        let settings: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["mcpServers"]["linear"]["command"], "npx");
        assert_eq!(settings["permissions"]["defaultMode"], "default");
        assert!(settings["hooks"].is_object());
        let rules = remove(&path, List::Allow, "Edit").unwrap();
        assert_eq!(rules.allow, ["Bash(git status:*)"]);
        assert_eq!(load(&path).unwrap(), rules);
    }

    #[test]
    fn no_file_is_no_rules_and_a_broken_one_is_left_alone() {
        let path = scratch("broken");
        assert_eq!(load(&path).unwrap(), Rules::default());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_err());
        assert!(add(&path, List::Allow, "git status").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        // A first rule makes the file.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(add(&path, List::Deny, "git push --force").unwrap().deny, ["Bash(git push --force:*)"]);
    }
}
