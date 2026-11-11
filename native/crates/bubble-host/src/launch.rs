//! Where the host program is and how to start it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// A command line that starts `bubble host --stdio`.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The whole environment of the process: the person's login shell's, so
    /// the agent's commands find what a terminal would.
    pub env: HashMap<String, String>,
}

impl Launch {
    /// Finds the host, in this order:
    ///
    /// 1. `BUBBLE_HOST_ENTRY`: Bubble's `dist/main.js`, run with Node.
    /// 2. The copy shipped inside the app (`Resources/bubble/dist/main.js`,
    ///    or `bubble/dist/main.js` beside the executable).
    /// 3. In a development build, the repository's own `dist/main.js`.
    /// 4. `bubble` on the PATH: the person's installed Bubble.
    ///
    /// Node is `BUBBLE_NODE`, the one shipped inside the app, or `node` on
    /// the PATH.
    pub fn find(env: &HashMap<String, String>) -> Result<Launch> {
        let path = env.get("PATH").cloned().unwrap_or_default();
        let executable = std::env::current_exe().ok();
        let beside = executable.as_deref().and_then(Path::parent);

        let mut entries: Vec<PathBuf> = Vec::new();
        if let Some(entry) = env.get("BUBBLE_HOST_ENTRY").filter(|e| !e.is_empty()) {
            // Named explicitly: a wrong path is an error, not a reason to look elsewhere.
            let entry = PathBuf::from(entry);
            if !entry.is_file() {
                bail!("BUBBLE_HOST_ENTRY points at {}, which does not exist", entry.display());
            }
            entries.push(entry);
        }
        if let Some(dir) = beside {
            entries.push(dir.join("../Resources/bubble/dist/main.js"));
            entries.push(dir.join("bubble/dist/main.js"));
        }
        if cfg!(debug_assertions) {
            entries.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dist/main.js"));
        }

        if let Some(entry) = entries.into_iter().find(|e| e.is_file()) {
            let bundled = beside.map(|dir| dir.join("../Resources/node/bin/node")).filter(|n| n.is_file());
            let node = env
                .get("BUBBLE_NODE")
                .filter(|n| !n.is_empty())
                .map(PathBuf::from)
                .or(bundled)
                .or_else(|| which("node", &path));
            let Some(node) = node else {
                bail!("Node.js was not found. Install Node 22 or newer, or set BUBBLE_NODE to its path.");
            };
            let entry = entry.canonicalize().unwrap_or(entry);
            return Ok(Launch {
                program: node,
                args: vec![entry.to_string_lossy().into_owned(), "host".into(), "--stdio".into()],
                env: env.clone(),
            });
        }

        match which("bubble", &path) {
            Some(bubble) => Ok(Launch { program: bubble, args: vec!["host".into(), "--stdio".into()], env: env.clone() }),
            None => bail!("Bubble's agent was not found. Install it with `npm install -g @bubblebrain-ai/bubble`."),
        }
    }
}

/// The first executable called `name` in the directories of `path`.
fn which(name: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path).map(|dir| dir.join(name)).find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_named_explicitly_must_exist() {
        let env = HashMap::from([("BUBBLE_HOST_ENTRY".to_owned(), "/nonexistent/main.js".to_owned())]);
        let error = Launch::find(&env).unwrap_err().to_string();
        assert!(error.contains("/nonexistent/main.js"), "{error}");
    }

    #[test]
    fn the_entry_runs_with_the_node_that_was_named() {
        let dir = std::env::temp_dir().join(format!("bubble-host-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("main.js");
        std::fs::write(&entry, "").unwrap();
        let env = HashMap::from([
            ("BUBBLE_HOST_ENTRY".to_owned(), entry.to_string_lossy().into_owned()),
            ("BUBBLE_NODE".to_owned(), "/opt/node/bin/node".to_owned()),
        ]);
        let launch = Launch::find(&env).unwrap();
        assert_eq!(launch.program, PathBuf::from("/opt/node/bin/node"));
        assert_eq!(launch.args[1..], ["host".to_owned(), "--stdio".to_owned()]);
        assert!(launch.args[0].ends_with("main.js"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
