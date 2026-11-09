//! What a run changed, as git sees it (the TypeScript CLI's
//! `agent/change-tracker.ts`): JSON mode reports it so a run cannot leave
//! out what it touched. The files dirty before the run are noted first;
//! afterwards the newly dirty ones are the run's, and the tracked test files
//! among them are listed with how many lines they lost.
//!
//! Everything is `None` outside a git repository or when git fails.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    bubble_core::detach(&mut command);
    let mut child = command.stdout(std::process::Stdio::piped()).spawn().ok()?;
    // Three seconds at most, as in TypeScript.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The paths `git status` shows as changed or untracked.
fn dirty(cwd: &Path) -> Option<BTreeSet<String>> {
    let out = git(cwd, &["status", "--porcelain", "-uall", "-z"])?;
    Some(out.split('\0').filter(|e| e.len() >= 4).map(|e| e[3..].to_owned()).collect())
}

/// The files dirty before a run.
pub struct Baseline(BTreeSet<String>);

pub fn baseline(cwd: &Path) -> Option<Baseline> {
    dirty(cwd).map(Baseline)
}

/// A test file, by its path (`tests/`, `__tests__/`, `spec/`, `*.test.*`,
/// `*_spec.*`, `test_*`).
pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('/').collect();
    if segments[..segments.len().saturating_sub(1)].iter().any(|s| matches!(*s, "test" | "tests" | "__tests__" | "spec")) {
        return true;
    }
    let name = segments.last().copied().unwrap_or_default();
    if name.starts_with("test_") {
        return true;
    }
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    ["test", "tests", "spec"]
        .iter()
        .any(|word| [".", "_", "-"].iter().any(|sep| stem.ends_with(&format!("{sep}{word}"))))
}

/// The run's changes since `baseline`: `{changed_files, modified_existing_tests}`.
pub fn detect(cwd: &Path, baseline: &Baseline) -> Option<Value> {
    let now = dirty(cwd)?;
    let changed: Vec<&String> = now.iter().filter(|p| !baseline.0.contains(*p)).collect();
    let mut tests = Vec::new();
    if !changed.is_empty() {
        let untracked: BTreeSet<String> = git(cwd, &["ls-files", "--others", "--exclude-standard", "-z"])
            .unwrap_or_default()
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let candidates: Vec<&str> =
            changed.iter().filter(|p| is_test_path(p) && !untracked.contains(**p)).map(|p| p.as_str()).collect();
        if !candidates.is_empty() {
            let mut args = vec!["diff", "HEAD", "--numstat", "-z", "--"];
            args.extend(candidates);
            if let Some(numstat) = git(cwd, &args) {
                for entry in numstat.split('\0') {
                    let parts: Vec<&str> = entry.split('\t').collect();
                    if let [_, deleted, path, ..] = parts.as_slice()
                        && let Ok(deleted) = deleted.parse::<u64>()
                        && !path.is_empty()
                    {
                        tests.push(json!({ "path": path, "deleted_lines": deleted }));
                    }
                }
            }
        }
    }
    Some(json!({ "changed_files": changed.len(), "modified_existing_tests": tests }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_are_recognized() {
        for path in ["tests/a.rs", "src/__tests__/x.ts", "a.test.ts", "b_spec.rb", "test_c.py", "spec/d.rb", "e-test.js"] {
            assert!(is_test_path(path), "{path}");
        }
        for path in ["src/main.rs", "latest.txt", "contest/a.rs", "testing.md"] {
            assert!(!is_test_path(path), "{path}");
        }
    }

    #[test]
    fn a_runs_changes_are_its_own() {
        let dir = std::env::temp_dir().join(format!("bubble-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(&dir).output().unwrap().status.success());
        run(&["init", "-q"]);
        std::fs::write(dir.join("tests/a.rs"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("dirty.txt"), "before").unwrap();
        run(&["add", "tests/a.rs"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        let base = baseline(&dir).unwrap();
        // The run: a test loses two lines, a new file appears; the file dirty
        // before is not the run's.
        std::fs::write(dir.join("tests/a.rs"), "one\n").unwrap();
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        std::fs::write(dir.join("dirty.txt"), "after").unwrap();
        let changes = detect(&dir, &base).unwrap();
        assert_eq!(changes["changed_files"], 2);
        assert_eq!(changes["modified_existing_tests"], json!([{ "path": "tests/a.rs", "deleted_lines": 2 }]));
        assert!(baseline(&std::env::temp_dir().join("bubble-no-repo-here-zz")).is_none());
    }
}
