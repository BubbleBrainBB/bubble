//! Git worktrees for write-capable subagents, as the TypeScript runtime
//! has them (`agent/worktree.ts`, `tools/child-tools.ts`): the child works
//! in a detached checkout of HEAD the runtime picks, never in the parent's
//! tree. What it may do there is decided by code, not by its prompt: files
//! only inside the worktree, commands only without pushing, remotes,
//! worktree management or sudo, and without absolute paths outside it.
//! When it finishes, an unchanged worktree is removed; a changed one is
//! kept for the parent to review and apply.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ToolCall;

/// A child's worktree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    /// The checkout.
    pub path: PathBuf,
    /// The repository it came from.
    pub root: PathBuf,
    /// Where in the checkout the child works (the parent's folder there).
    pub cwd: PathBuf,
    /// Set when the child finished: whether it left changes.
    pub changed: Option<bool>,
    /// `git diff --stat` and the new files, when it did.
    pub diff_stat: String,
    /// It was unchanged and is gone.
    pub removed: bool,
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(dir).args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    crate::detach_async(&mut command);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| format!("git {} took too long", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A detached worktree at HEAD of the repository `cwd` is in.
pub async fn create(cwd: &Path, agent_id: &str) -> Result<Worktree, String> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"])
        .await
        .map(|out| PathBuf::from(out.trim()))
        .map_err(|_| "write_worktree subagents need a git repository: the working directory is not inside one.".to_owned())?;
    let path = std::env::temp_dir().join(format!("bubble-wt-{agent_id}-{}", crate::time::unique_id()));
    let target = path.display().to_string();
    git(&root, &["worktree", "add", "--detach", &target, "HEAD"])
        .await
        .map_err(|e| format!("Failed to create subagent worktree: {e}"))?;
    // The child works where the parent does, inside the checkout.
    let relative = cwd
        .canonicalize()
        .ok()
        .zip(root.canonicalize().ok())
        .and_then(|(cwd, root)| cwd.strip_prefix(root).ok().map(Path::to_path_buf))
        .unwrap_or_default();
    let child_cwd = path.join(relative);
    let _ = std::fs::create_dir_all(&child_cwd);
    Ok(Worktree { path, root, cwd: child_cwd, changed: None, diff_stat: String::new(), removed: false })
}

/// Looks at a finished child's worktree: unchanged ones are removed,
/// changed ones are kept with a summary of the changes.
pub async fn finish(worktree: &mut Worktree) {
    if worktree.changed.is_some() {
        return;
    }
    let status = git(&worktree.path, &["status", "--porcelain"]).await.unwrap_or_default();
    let stat = git(&worktree.path, &["diff", "HEAD", "--stat"]).await.unwrap_or_default();
    let new_files: Vec<String> =
        status.lines().filter_map(|l| l.strip_prefix("?? ")).map(|f| format!("  new file: {f}")).collect();
    worktree.changed = Some(!status.trim().is_empty());
    worktree.diff_stat = [stat.trim().to_owned()].into_iter().chain(new_files).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    if worktree.changed == Some(false) {
        let target = worktree.path.display().to_string();
        worktree.removed = git(&worktree.root, &["worktree", "remove", "--force", &target]).await.is_ok();
    }
}

/// `path` with `.` and `..` worked out, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Whether `candidate` (relative to `cwd`) is inside `root`.
pub fn inside(root: &Path, cwd: &Path, candidate: &str) -> bool {
    let candidate = Path::new(candidate.trim());
    let full = normalize(&if candidate.is_absolute() { candidate.to_owned() } else { cwd.join(candidate) });
    let roots = [Some(normalize(root)), root.canonicalize().ok()];
    // The same place under another name (macOS's /var is /private/var).
    let full_real = full.parent().and_then(|p| p.canonicalize().ok()).map(|p| p.join(full.file_name().unwrap_or_default()));
    roots.iter().flatten().any(|r| full.starts_with(r) || full_real.as_ref().is_some_and(|f| f.starts_with(r)))
}

/// Why a worktree child may not make `call`, or `None` when it may.
pub fn refusal(call: &ToolCall, root: &Path, cwd: &Path) -> Option<&'static str> {
    let args: Value = call.arguments_json();
    match call.name.as_str() {
        "bash" => {
            let command = args["command"].as_str().unwrap_or_default();
            let denied: [(&str, &'static str); 4] = [
                (r"\bgit\s+push\b", "Blocked by worktree policy: pushing from a subagent worktree is not allowed; the parent reviews and applies changes."),
                (r"\bgit\s+remote\b", "Blocked by worktree policy: remote configuration is not allowed inside a subagent worktree."),
                (r"\bgit\s+worktree\b", "Blocked by worktree policy: managing worktrees from inside a subagent worktree is not allowed."),
                (r"\bsudo\b", "Blocked by worktree policy: privileged commands are not allowed inside a subagent worktree."),
            ];
            for (pattern, reason) in denied {
                if Regex::new(pattern).is_ok_and(|r| r.is_match(command)) {
                    return Some(reason);
                }
            }
            let paths = Regex::new(r#"(?:^|[\s"'=])(/[^\s"';|&]+)"#).expect("valid");
            for found in paths.captures_iter(command) {
                let path = &found[1];
                let system = ["/dev/", "/tmp/", "/usr/", "/bin/", "/opt/", "/etc/"].iter().any(|p| path.starts_with(p));
                if !system && !inside(root, cwd, path) {
                    return Some("Blocked by worktree policy: the command references a path outside the worktree.");
                }
            }
            None
        }
        "write" | "edit" => {
            let path = args["path"].as_str().unwrap_or_default();
            (!inside(root, cwd, path)).then_some("Blocked by worktree policy: the path is outside the subagent worktree.")
        }
        _ => Some("Blocked by worktree policy: subagents cannot approve this tool."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall { id: "c".into(), name: name.into(), arguments: arguments.to_string() }
    }

    #[test]
    fn the_policy_keeps_children_inside() {
        let root = Path::new("/tmp/wt");
        let cwd = Path::new("/tmp/wt/app");
        let ok = |c: &ToolCall| refusal(c, root, cwd).is_none();
        assert!(ok(&call("write", serde_json::json!({ "path": "src/a.rs" }))));
        assert!(ok(&call("edit", serde_json::json!({ "path": "/tmp/wt/b.rs" }))));
        assert!(!ok(&call("write", serde_json::json!({ "path": "../../etc/passwd" }))));
        assert!(!ok(&call("write", serde_json::json!({ "path": "/Users/me/project/a.rs" }))));
        assert!(ok(&call("bash", serde_json::json!({ "command": "cargo test && ls /usr/bin" }))));
        assert!(!ok(&call("bash", serde_json::json!({ "command": "git push origin main" }))));
        assert!(!ok(&call("bash", serde_json::json!({ "command": "sudo rm -rf x" }))));
        assert!(!ok(&call("bash", serde_json::json!({ "command": "cat /Users/me/.ssh/id_rsa" }))));
        assert!(ok(&call("bash", serde_json::json!({ "command": "cat /tmp/wt/app/x" }))));
        assert!(!ok(&call("mcp__x__y", serde_json::json!({}))));
    }

    #[tokio::test]
    async fn worktrees_are_made_and_tidied() {
        let repo = std::env::temp_dir().join(format!("bubble-wt-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("app")).unwrap();
        let run = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&repo).output().unwrap().status.success());
        run(&["init", "-q"]);
        std::fs::write(repo.join("app/a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

        let mut unchanged = create(&repo.join("app"), "agent_1").await.unwrap();
        assert!(unchanged.cwd.ends_with("app") && unchanged.cwd.join("a.txt").exists(), "the child starts in the same folder");
        finish(&mut unchanged).await;
        assert_eq!(unchanged.changed, Some(false));
        assert!(unchanged.removed && !unchanged.path.exists());

        let mut changed = create(&repo, "agent_2").await.unwrap();
        std::fs::write(changed.path.join("app/a.txt"), "two\n").unwrap();
        std::fs::write(changed.path.join("new.txt"), "x").unwrap();
        finish(&mut changed).await;
        assert_eq!(changed.changed, Some(true));
        assert!(changed.diff_stat.contains("app/a.txt") && changed.diff_stat.contains("new file: new.txt"), "{}", changed.diff_stat);
        assert!(changed.path.exists(), "kept for review");
        assert_eq!(std::fs::read_to_string(repo.join("app/a.txt")).unwrap(), "one\n", "the parent's tree is untouched");
        assert!(create(&std::env::temp_dir(), "agent_3").await.unwrap_err().contains("git repository"));
    }
}
