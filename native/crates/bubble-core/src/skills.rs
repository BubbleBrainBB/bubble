//! Skills: folders with a `SKILL.md` (name and description in front matter,
//! instructions below). The system prompt lists them; the `skill` tool loads
//! one's instructions when a task matches (src/skills/ in the TypeScript core).
//!
//! Looked for in `~/.bubble/skills`, `~/.agents/skills`, `~/.claude/skills`,
//! the project's `.bubble/skills` and config.json's `skillPaths`; a name seen
//! twice keeps the first.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::provider::ToolSpec;
use crate::tools::ToolOutput;

/// The catalog's budget for its listed lines.
pub const CATALOG_CHARS: usize = 12_000;
/// Each description is cut to this; it only has to say when to load.
const CATALOG_DESC_CHARS: usize = 120;
const CATALOG_INTRO: &str = "Skills are task-specific instructions loaded on demand. When a task matches a skill below, call `skill` with its exact name before starting, then follow it. Load only what clearly applies.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Project,
    Configured,
    User,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Project => "project",
            Source::Configured => "configured",
            Source::User => "user",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Hidden from the model; a person can still use it.
    pub manual_only: bool,
    pub root: PathBuf,
    pub content: String,
    pub resources: Vec<String>,
    pub source: Source,
}

/// The skills found, and the files that could not be used.
#[derive(Clone, Debug, Default)]
pub struct Skills {
    skills: Vec<Skill>,
    disabled: HashSet<String>,
    pub diagnostics: Vec<String>,
    /// Skills skipped because one of the same name came first (often the
    /// same skill in both ~/.agents and ~/.claude).
    pub shadowed: usize,
}

impl Skills {
    /// The usual places, for a project (or none).
    pub fn roots(home: &Path, project: Option<&Path>, configured: &[String]) -> Vec<(PathBuf, Source)> {
        let user_home = dirs::home_dir().unwrap_or_default();
        let mut roots = vec![
            (home.join("skills"), Source::User),
            (user_home.join(".agents").join("skills"), Source::User),
            (user_home.join(".claude").join("skills"), Source::User),
        ];
        if let Some(project) = project {
            roots.push((project.join(".bubble").join("skills"), Source::Project));
        }
        roots.extend(configured.iter().map(|p| (PathBuf::from(p), Source::Configured)));
        roots
    }

    pub fn discover(roots: &[(PathBuf, Source)], disabled: &[String]) -> Skills {
        let mut found = Skills { disabled: disabled.iter().cloned().collect(), ..Skills::default() };
        let mut seen: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (root, source) in roots {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
            dirs.sort();
            for dir in dirs {
                // Follows links, as many skills are linked in.
                if !std::fs::metadata(&dir).is_ok_and(|m| m.is_dir()) {
                    continue;
                }
                let file = dir.join("SKILL.md");
                let Ok(raw) = std::fs::read_to_string(&file) else { continue };
                match load(&raw, &dir, *source) {
                    Ok(skill) => {
                        if seen.contains_key(&skill.name) {
                            found.shadowed += 1;
                            continue;
                        }
                        seen.insert(skill.name.clone(), file);
                        found.skills.push(skill);
                    }
                    Err(message) => found.diagnostics.push(format!("{}: {message}", file.display())),
                }
            }
        }
        found.skills.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    pub fn all(&self) -> &[Skill] {
        &self.skills
    }

    /// The skills a model may see and load.
    pub fn visible(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter().filter(|s| !s.manual_only && !self.disabled.contains(&s.name))
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        if self.disabled.contains(name) {
            return None;
        }
        self.skills.iter().find(|s| s.name == name)
    }

    /// The system prompt's list of skills, or `None` with nothing to list.
    pub fn catalog(&self, budget: usize) -> Option<String> {
        let mut sorted: Vec<&Skill> = self.visible().collect();
        if sorted.is_empty() || budget == 0 {
            return None;
        }
        // Stable order keeps the prompt byte-identical, and cacheable.
        sorted.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.name.cmp(&b.name)));
        let mut lines = Vec::new();
        let mut used = 0;
        for skill in &sorted {
            let line = catalog_line(skill);
            let len = line.chars().count();
            if used + len + 1 > budget {
                break;
            }
            used += len + 1;
            lines.push(line);
        }
        if lines.is_empty() {
            return None;
        }
        let unlisted = sorted.len() - lines.len();
        if unlisted > 0 {
            lines.push(format!("- … and {unlisted} more skills not listed here; use skill_search to find them."));
        }
        Some(format!("## Skills\n{CATALOG_INTRO}\n{}", lines.join("\n")))
    }

    pub fn specs() -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: "skill".into(),
                description: "Load a skill by its exact name, as listed in the Skills section of the system prompt. Returns the skill's instructions; follow them for the task.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "name": { "type": "string", "description": "The exact skill name to load" } },
                    "required": ["name"],
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: "skill_search".into(),
                description: "Search available skills by name, description, tags, and source. Use this before loading a skill when a task may match a specialized workflow.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search terms describing the desired skill or workflow." },
                        "max_results": { "type": "number", "description": "Maximum number of matches to return (default 8, max 25)." },
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                }),
            },
        ]
    }

    /// The `skill` tool.
    pub fn load_tool(&self, args: &Value) -> ToolOutput {
        let name = args["name"].as_str().unwrap_or_default().trim();
        if name.is_empty() {
            return ToolOutput { content: "Error: skill name is required".into(), is_error: true };
        }
        match self.get(name) {
            Some(skill) => ToolOutput { content: loaded(skill), is_error: false },
            None => {
                let names: Vec<&str> = self.visible().map(|s| s.name.as_str()).collect();
                let close = closest(name, &names);
                ToolOutput {
                    content: format!(
                        "Error: Unknown skill \"{name}\".{} Use the exact name from the Skills section.",
                        if close.is_empty() { String::new() } else { format!(" Did you mean: {}?", close.join(", ")) }
                    ),
                    is_error: true,
                }
            }
        }
    }

    /// The `skill_search` tool.
    pub fn search_tool(&self, args: &Value) -> ToolOutput {
        let query = args["query"].as_str().unwrap_or_default().trim();
        let max = args["max_results"].as_f64().filter(|n| *n > 0.0).map(|n| (n as usize).min(25)).unwrap_or(8);
        let visible: Vec<&Skill> = self.visible().collect();
        if visible.is_empty() {
            return ToolOutput { content: "No skills are currently available.".into(), is_error: false };
        }
        let matches = search(&visible, query);
        if matches.is_empty() {
            return ToolOutput {
                content: format!("No skills matched \"{query}\". Try broader terms or use /skills to browse all skills manually."),
                is_error: false,
            };
        }
        let shown: Vec<&Skill> = matches.into_iter().take(max).collect();
        let mut lines = vec![if query.is_empty() {
            format!("Available skills ({} of {}):", shown.len(), visible.len())
        } else {
            format!("Skill search results for \"{query}\" ({} of {}):", shown.len(), visible.len())
        }];
        for skill in &shown {
            let tags = if skill.tags.is_empty() { String::new() } else { format!(" [tags: {}]", skill.tags.join(", ")) };
            lines.push(format!("- {} ({}): {}{tags}", skill.name, skill.source.label(), skill.description));
        }
        lines.push(String::new());
        lines.push("Call skill with the exact name to load a selected skill.".into());
        ToolOutput { content: lines.join("\n"), is_error: false }
    }
}

fn catalog_line(skill: &Skill) -> String {
    let raw = skill.description.split_whitespace().collect::<Vec<_>>().join(" ");
    let desc = if raw.chars().count() > CATALOG_DESC_CHARS {
        let cut: String = raw.chars().take(CATALOG_DESC_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        raw
    };
    if desc.is_empty() { format!("- {}", skill.name) } else { format!("- {}: {desc}", skill.name) }
}

/// What the `skill` tool returns.
fn loaded(skill: &Skill) -> String {
    let mut sections = vec![
        format!("Skill: {}", skill.name),
        format!("Description: {}", skill.description),
        format!("Base directory: {}", skill.root.display()),
        String::new(),
        skill.content.clone(),
    ];
    if !skill.resources.is_empty() {
        sections.push(String::new());
        sections.push("Resources:".into());
        sections.extend(skill.resources.iter().map(|r| format!("- {r}")));
    }
    sections.push(String::new());
    sections.push("Relative paths mentioned in this skill are resolved from the base directory above.".into());
    sections.join("\n")
}

fn load(raw: &str, root: &Path, source: Source) -> Result<Skill, String> {
    let (attributes, body) = front_matter(raw);
    let fallback = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = attributes.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&fallback).to_owned();
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !valid {
        return Err(format!("Invalid skill name \"{name}\""));
    }
    let description = attributes.get("description").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if description.is_empty() {
        return Err("Skill description is required".into());
    }
    let tags = attributes
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| t.iter().filter_map(|v| v.as_str().map(str::trim)).filter(|t| !t.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let mut resources = Vec::new();
    for folder in ["references", "scripts", "assets"] {
        let mut files = Vec::new();
        walk(&root.join(folder), root, &mut files);
        files.sort();
        resources.extend(files);
    }
    Ok(Skill {
        name,
        description: description.to_owned(),
        tags,
        manual_only: attributes.get("disable-model-invocation") == Some(&Value::Bool(true)),
        root: root.to_path_buf(),
        content: body.trim().to_owned(),
        resources,
        source,
    })
}

fn walk(dir: &Path, root: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, root, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// The simple YAML front matter skills use: `key: value` and lists of
/// `- item` under an empty key.
pub fn front_matter(raw: &str) -> (BTreeMap<String, Value>, String) {
    if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
        return (BTreeMap::new(), raw.to_owned());
    }
    let normalized = raw.replace("\r\n", "\n");
    let Some(end) = normalized[4..].find("\n---\n").map(|i| i + 4) else {
        return (BTreeMap::new(), raw.to_owned());
    };
    let block = &normalized[4..end];
    let body = normalized[end + 5..].to_owned();
    let mut attributes = BTreeMap::new();
    let mut list_key: Option<String> = None;
    let lines: Vec<&str> = block.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if line.trim().is_empty() {
            continue;
        }
        let trimmed = line.trim_start();
        if let (Some(item), Some(key)) = (trimmed.strip_prefix("- "), &list_key) {
            if let Some(Value::Array(items)) = attributes.get_mut(key) {
                items.push(scalar(item.trim()));
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            list_key = None;
            continue;
        };
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            list_key = None;
            continue;
        }
        let value = value.trim();
        // Block scalars (`|` keeps lines, `>` folds them), which many
        // skills use for long descriptions.
        if let Some(style) = value.chars().next().filter(|c| matches!(c, '|' | '>')).filter(|_| value.len() <= 2) {
            let mut block_lines = Vec::new();
            while index < lines.len() && (lines[index].trim().is_empty() || lines[index].starts_with([' ', '\t'])) {
                block_lines.push(lines[index]);
                index += 1;
            }
            let indent = block_lines
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            let dedented: Vec<&str> =
                block_lines.iter().map(|l| if l.trim().is_empty() { "" } else { &l[indent..] }).collect();
            let text = if style == '|' {
                dedented.join("\n")
            } else {
                dedented.split(|l| l.is_empty()).map(|para| para.join(" ")).collect::<Vec<_>>().join("\n")
            };
            attributes.insert(key.to_owned(), Value::String(text.trim_end().to_owned()));
            list_key = None;
            continue;
        }
        if value.is_empty() {
            attributes.insert(key.to_owned(), Value::Array(Vec::new()));
            list_key = Some(key.to_owned());
        } else {
            attributes.insert(key.to_owned(), scalar(value));
            list_key = None;
        }
    }
    (attributes, body)
}

fn scalar(value: &str) -> Value {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    match unquoted {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        number if number.parse::<i64>().is_ok() && !number.starts_with('+') => json!(number.parse::<i64>().unwrap()),
        text => Value::String(text.to_owned()),
    }
}

fn is_cjk(c: char) -> bool {
    ('\u{3000}'..='\u{9fff}').contains(&c)
}

fn terms(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut terms: Vec<String> = Vec::new();
    for term in lower.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || is_cjk(c))) {
        if term.is_empty() {
            continue;
        }
        if !terms.contains(&term.to_owned()) {
            terms.push(term.to_owned());
        }
        let chars: Vec<char> = term.chars().collect();
        if chars.iter().any(|c| is_cjk(*c)) && chars.len() > 2 {
            for pair in chars.windows(2) {
                let pair: String = pair.iter().collect();
                if !terms.contains(&pair) {
                    terms.push(pair);
                }
            }
        }
    }
    terms
}

fn score(skill: &Skill, terms: &[String], query: &str) -> usize {
    let name = skill.name.to_lowercase();
    let desc = skill.description.to_lowercase();
    let tags: Vec<String> = skill.tags.iter().map(|t| t.to_lowercase()).collect();
    let source = skill.source.label();
    let bonus = match skill.source {
        Source::Project => 4,
        Source::Configured => 2,
        Source::User => 0,
    };
    if terms.is_empty() {
        return 1 + bonus;
    }
    let query = query.trim().to_lowercase();
    let mut score = 0;
    if name == query {
        score += 80;
    }
    if !query.is_empty() && name.contains(&query) {
        score += 30;
    }
    for term in terms {
        if &name == term {
            score += 30;
        } else if name.contains(term.as_str()) {
            score += 12;
        }
        if tags.iter().any(|t| t == term) {
            score += 10;
        } else if tags.iter().any(|t| t.contains(term.as_str())) {
            score += 6;
        }
        if desc.contains(term.as_str()) {
            score += 3;
        }
        if source.contains(term.as_str()) {
            score += 2;
        }
    }
    if score > 0 { score + bonus } else { 0 }
}

fn search<'a>(skills: &[&'a Skill], query: &str) -> Vec<&'a Skill> {
    let terms = terms(query);
    let mut scored: Vec<(usize, &Skill)> =
        skills.iter().map(|s| (score(s, &terms, query), *s)).filter(|(score, _)| *score > 0 || terms.is_empty()).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.source.cmp(&b.1.source)).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, s)| s).collect()
}

/// Up to three names close to a mistyped one.
pub fn closest(name: &str, names: &[&str]) -> Vec<String> {
    let target = name.to_lowercase();
    let limit = 2.max(target.chars().count() / 3);
    let mut scored: Vec<(usize, &str)> = names
        .iter()
        .map(|candidate| {
            let lower = candidate.to_lowercase();
            let contains = target.chars().count() >= 3 && (lower.contains(&target) || target.contains(&lower));
            (if contains { 0 } else { edit_distance(&target, &lower) }, *candidate)
        })
        .filter(|(score, _)| *score <= limit)
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, n)| n.to_owned()).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 1..=b.len() {
            let current = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(ca != b[j - 1]));
            prev = current;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bubble-skills-{}-{}", std::process::id(), NEXT_DIR.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn skill(root: &Path, folder: &str, text: &str) {
        std::fs::create_dir_all(root.join(folder)).unwrap();
        std::fs::write(root.join(folder).join("SKILL.md"), text).unwrap();
    }

    #[test]
    fn front_matter_reads_scalars_and_lists() {
        let (attributes, body) = front_matter("---\nname: \"pdf\"\nversion: 2\nhidden: true\ntags:\n  - docs\n  - files\n---\nBody\n");
        assert_eq!(attributes["name"], "pdf");
        assert_eq!(attributes["version"], 2);
        assert_eq!(attributes["hidden"], true);
        assert_eq!(attributes["tags"], json!(["docs", "files"]));
        assert_eq!(body, "Body\n");
        assert_eq!(front_matter("no front matter").1, "no front matter");
        let (block, _) = front_matter("---\nname: x\ndescription: |\n  First line,\n    indented.\n\n  After a gap.\nfolded: >-\n  one\n  two\n\n  three\nnext: 1\n---\n");
        assert_eq!(block["description"], "First line,\n  indented.\n\nAfter a gap.");
        assert_eq!(block["folded"], "one two\nthree");
        assert_eq!(block["next"], 1);
    }

    #[test]
    fn discovery_keeps_the_first_name_and_skips_bad_ones() {
        let user = temp();
        let project = temp();
        skill(&user, "pdf", "---\nname: pdf\ndescription: Work with PDFs\n---\nUse pdftotext.");
        std::fs::create_dir_all(user.join("pdf/scripts/sub")).unwrap();
        std::fs::write(user.join("pdf/scripts/sub/run.sh"), "").unwrap();
        skill(&user, "nodesc", "---\nname: nodesc\n---\nx");
        skill(&user, "secret", "---\ndescription: Manual only\ndisable-model-invocation: true\n---\nx");
        skill(&project, "pdf", "---\nname: pdf\ndescription: Shadowed\n---\nx");
        let skills = Skills::discover(&[(user.clone(), Source::User), (project, Source::Project)], &[]);
        assert_eq!(skills.all().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["pdf", "secret"]);
        assert_eq!(skills.all()[0].resources, ["scripts/sub/run.sh"]);
        assert_eq!(skills.diagnostics.len(), 1, "the skill with no description");
        assert_eq!(skills.shadowed, 1);
        assert_eq!(skills.visible().count(), 1);

        let loaded = skills.load_tool(&json!({ "name": "pdf" }));
        assert!(loaded.content.starts_with("Skill: pdf\nDescription: Work with PDFs\nBase directory: "));
        assert!(loaded.content.contains("Use pdftotext.\n\nResources:\n- scripts/sub/run.sh"));
        let typo = skills.load_tool(&json!({ "name": "pfd" }));
        assert!(typo.is_error && typo.content.contains("Did you mean: pdf?"));

        let disabled = Skills::discover(&[(user, Source::User)], &["pdf".into()]);
        assert!(disabled.get("pdf").is_none());
        assert!(disabled.catalog(CATALOG_CHARS).is_none());
    }

    #[test]
    fn catalogs_sort_by_source_and_respect_the_budget() {
        let user = temp();
        let project = temp();
        skill(&user, "alpha", &format!("---\ndescription: {}\n---\n", "word ".repeat(40)));
        skill(&user, "beta", "---\ndescription: Second\n---\n");
        skill(&project, "zeta", "---\ndescription: From the project\n---\n");
        let skills = Skills::discover(&[(user, Source::User), (project, Source::Project)], &[]);
        let catalog = skills.catalog(CATALOG_CHARS).unwrap();
        let lines: Vec<&str> = catalog.lines().collect();
        assert_eq!(lines[0], "## Skills");
        assert_eq!(lines[2], "- zeta: From the project");
        assert!(lines[3].starts_with("- alpha: word word") && lines[3].ends_with('…'));
        assert_eq!(lines[3].chars().count(), "- alpha: ".len() + CATALOG_DESC_CHARS);
        let small = skills.catalog(30).unwrap();
        assert!(small.ends_with("- … and 2 more skills not listed here; use skill_search to find them."));
    }

    #[test]
    fn search_ranks_names_tags_and_cjk_pairs() {
        let root = temp();
        skill(&root, "lark-doc", "---\ndescription: 飞书云文档读取和编辑\ntags:\n  - feishu\n---\n");
        skill(&root, "pdf", "---\ndescription: PDF tools for documents\n---\n");
        let skills = Skills::discover(&[(root, Source::User)], &[]);
        let found = skills.search_tool(&json!({ "query": "pdf" }));
        assert!(found.content.lines().nth(1).unwrap().starts_with("- pdf (user): PDF tools"));
        let cjk = skills.search_tool(&json!({ "query": "编辑文档" }));
        assert!(cjk.content.contains("- lark-doc"));
        let tag = skills.search_tool(&json!({ "query": "feishu" }));
        assert!(tag.content.contains("[tags: feishu]"));
        assert!(skills.search_tool(&json!({ "query": "zzzz" })).content.starts_with("No skills matched"));
    }
}
