//! The composer's helpers, as the TypeScript TUI has them: the prompts
//! typed before (`~/.bubble/input-history.jsonl`, shared with it) and
//! `@file` mentions (completed from the project's files, their contents
//! sent along).

use std::io::Write;
use std::path::Path;

use bubble_core::bubble_home;
use serde_json::{Value, json};

const HISTORY_LIMIT: usize = 1000;
/// Files larger than this are named, not inlined.
const INLINE_LIMIT: u64 = 200 * 1024;
const FILE_LIMIT: usize = 50_000;

fn history_path() -> std::path::PathBuf {
    bubble_home().join("input-history.jsonl")
}

/// The prompts typed in `cwd`, oldest first, without repeats in a row.
pub fn history(cwd: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(history_path()) else { return Vec::new() };
    let mut entries: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let Some(prompt) = entry["text"].as_str().filter(|t| !t.trim().is_empty()) else { continue };
        if entry["cwd"].as_str().is_some_and(|c| c != cwd) {
            continue;
        }
        if entries.last().map(String::as_str) != Some(prompt) {
            entries.push(prompt.to_owned());
        }
    }
    let skip = entries.len().saturating_sub(HISTORY_LIMIT);
    entries.split_off(skip)
}

/// Keeps `text` for the next time.
pub fn remember(text: &str, cwd: &str) {
    let entry = json!({ "text": text, "createdAt": bubble_core::time::now_utc().iso, "cwd": cwd });
    let path = history_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{entry}");
    }
}

/// The project's files (relative, `/`-separated), as `.gitignore` leaves
/// them.
pub fn list(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(cwd)
        .hidden(true)
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some("node_modules" | "dist" | "build" | ".next" | "target")))
        .build();
    for entry in walker.flatten() {
        if files.len() >= FILE_LIMIT {
            break;
        }
        if entry.file_type().is_some_and(|t| t.is_file())
            && let Ok(relative) = entry.path().strip_prefix(cwd)
        {
            files.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    files.sort();
    files
}

/// The files matching `query`: name starts, then path starts, then name
/// and path contain it.
pub fn matching(files: &[String], query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, &String)> = files
        .iter()
        .filter_map(|file| {
            let lower = file.to_lowercase();
            let base = lower.rsplit('/').next().unwrap_or(&lower);
            let score = if q.is_empty() {
                1
            } else if base.starts_with(&q) {
                100
            } else if lower.starts_with(&q) {
                80
            } else if base.contains(&q) {
                60
            } else if lower.contains(&q) {
                40
            } else {
                0
            };
            (score > 0).then_some((score, file))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, f)| f.clone()).collect()
}

/// The `@` word the draft ends in: where it starts, and what follows the `@`.
pub fn mention_at_end(draft: &str) -> Option<(usize, &str)> {
    let at = draft.rfind('@')?;
    let before = draft[..at].chars().last();
    if before.is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let query = &draft[at + 1..];
    (!query.contains(char::is_whitespace)).then_some((at, query))
}

/// `text` with the files it mentions (`@path`) appended, as the TypeScript
/// TUI sends them.
pub fn expand(text: &str, cwd: &Path) -> String {
    let mut blocks = Vec::new();
    let mut seen = Vec::new();
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
    for word in text.split_whitespace() {
        let Some(token) = word.strip_prefix('@').filter(|t| !t.is_empty()) else { continue };
        if seen.contains(&token) {
            continue;
        }
        seen.push(token);
        let Ok(path) = root.join(token).canonicalize() else { continue };
        if !path.starts_with(&root) || !path.is_file() {
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > INLINE_LIMIT {
            blocks.push(format!(
                "### @{token}\n({:.1}KB, exceeds inline limit of 200.0KB — use the Read tool to access)",
                size as f64 / 1024.0
            ));
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        let language = language(token);
        blocks.push(format!("### @{token}\n```{language}\n{contents}\n```"));
    }
    if blocks.is_empty() {
        return text.to_owned();
    }
    format!("{text}\n\n---\nReferenced files:\n\n{}", blocks.join("\n\n"))
}

fn language(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default().to_lowercase().as_str() {
        "ts" => "ts",
        "tsx" => "tsx",
        "js" => "js",
        "jsx" => "jsx",
        "py" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "sh" | "zsh" | "bash" => "bash",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "html" => "html",
        "css" => "css",
        "scss" => "scss",
        "sql" => "sql",
        "md" => "markdown",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_complete_and_expand() {
        assert_eq!(mention_at_end("look at @src/ma"), Some((8, "src/ma")));
        assert_eq!(mention_at_end("mail me@host"), None);
        assert_eq!(mention_at_end("@a b"), None);
        let files = vec!["src/main.rs".to_owned(), "docs/main-notes.md".to_owned(), "README.md".to_owned()];
        assert_eq!(matching(&files, "main", 5), ["src/main.rs", "docs/main-notes.md"]);
        assert_eq!(matching(&files, "read", 5), ["README.md"]);

        let dir = std::env::temp_dir().join(format!("bubble-mentions-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "fn main() {}").unwrap();
        let expanded = expand("explain @src/lib.rs and @missing.txt", &dir);
        assert!(expanded.starts_with("explain @src/lib.rs"));
        assert!(expanded.contains("### @src/lib.rs\n```rust\nfn main() {}\n```"));
        assert!(!expanded.contains("### @missing.txt"));
        assert_eq!(expand("no mentions", &dir), "no mentions");
        assert_eq!(list(&dir), ["src/lib.rs"]);
        assert_eq!(expand("@../../etc/passwd", &dir.join("src")), "@../../etc/passwd", "outside the project");
    }
}
