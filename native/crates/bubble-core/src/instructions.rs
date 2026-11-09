//! The person's standing instructions: `AGENTS.md` in Bubble's home, at the
//! project's root and in its `.bubble` folder, added to the system prompt as
//! the TypeScript core does (`memory/store.ts`).

use std::path::{Path, PathBuf};

use crate::config::bubble_home;

const MAX_FILE_CHARS: usize = 12_000;

/// The project's root: the nearest folder upward with a `.git`, else
/// `cwd` itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut dir = Some(cwd);
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return current.to_owned();
        }
        dir = current.parent();
    }
    cwd.to_owned()
}

/// The instructions for work in `cwd` (none: Bubble's own only), or `None`
/// when there are none.
pub fn instructions(cwd: Option<&Path>) -> Option<String> {
    instructions_with_home(&bubble_home(), cwd)
}

pub fn instructions_with_home(home: &Path, cwd: Option<&Path>) -> Option<String> {
    let mut files = vec![("Global AGENTS.md", home.join("AGENTS.md"))];
    if let Some(cwd) = cwd {
        let root = project_root(cwd);
        files.push(("Project AGENTS.md", root.join("AGENTS.md")));
        files.push(("Project .bubble/AGENTS.md", root.join(".bubble").join("AGENTS.md")));
    }
    let sections: Vec<String> = files
        .into_iter()
        .filter_map(|(label, path)| {
            let content = std::fs::read_to_string(&path).ok()?;
            let content = content.trim();
            (!content.is_empty()).then(|| format!("### {label}\nPath: {}\n\n{}", path.display(), truncate(content)))
        })
        .collect();
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn truncate(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    if chars.len() <= MAX_FILE_CHARS {
        return content.to_owned();
    }
    let kept: String = chars[..MAX_FILE_CHARS - 80].iter().collect();
    format!("{}\n\n[truncated {} chars]", kept.trim_end(), chars.len() - MAX_FILE_CHARS + 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_and_project_files_join_from_the_git_root() {
        let root = std::env::temp_dir().join(format!("bubble-instructions-{}", std::process::id()));
        let home = root.join("home");
        let project = root.join("project");
        let nested = project.join("src").join("deep");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join(".bubble")).unwrap();
        std::fs::write(home.join("AGENTS.md"), "Answer in Chinese.\n").unwrap();
        std::fs::write(project.join("AGENTS.md"), "Run cargo test before saying done.").unwrap();
        std::fs::write(project.join(".bubble").join("AGENTS.md"), "   ").unwrap();

        let text = instructions_with_home(&home, Some(&nested)).unwrap();
        assert!(text.starts_with("### Global AGENTS.md\nPath: "));
        assert!(text.contains("Answer in Chinese."));
        assert!(text.contains("### Project AGENTS.md"));
        assert!(text.contains("Run cargo test before saying done."));
        assert!(!text.contains(".bubble/AGENTS.md"), "empty files are left out");

        let manager = instructions_with_home(&home, None).unwrap();
        assert!(!manager.contains("Project"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn long_files_are_cut_with_a_note() {
        let text = truncate(&"x".repeat(MAX_FILE_CHARS + 500));
        assert!(text.ends_with("[truncated 580 chars]"));
    }
}
