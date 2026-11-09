//! `bubble update` (`upgrade`): the npm package's latest version, installed
//! with the package manager this copy came from, as the TypeScript CLI does
//! (`src/update/`). `--check` only says whether there is one.

use std::path::Path;
use std::time::Duration;

pub const PACKAGE: &str = "@bubblebrain-ai/bubble";

/// This build's version: the npm package's when built for it.
pub fn version() -> &'static str {
    option_env!("BUBBLE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// `a` against `b` by their numbers (`1.2.10` > `1.2.9`); a pre-release
/// counts as before its release.
pub fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p.to_owned())));
        let numbers: Vec<u64> = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        (numbers, pre)
    };
    let (an, ap) = parse(a);
    let (bn, bp) = parse(b);
    let width = an.len().max(bn.len());
    for i in 0..width {
        let ordering = an.get(i).unwrap_or(&0).cmp(bn.get(i).unwrap_or(&0));
        if ordering.is_ne() {
            return ordering;
        }
    }
    match (ap, bp) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Npm,
    Bun,
    Pnpm,
    Yarn,
    Homebrew,
    /// A build from source (`cargo build`).
    Source,
}

/// How the copy at `path` (the binary, links resolved) was installed.
pub fn manager_of(path: &Path) -> Manager {
    let lower = path.display().to_string().replace('\\', "/").to_lowercase();
    if lower.contains("/.bun/") || lower.contains("/bun/install/") {
        Manager::Bun
    } else if lower.contains("/pnpm/") || lower.contains("/.pnpm/") {
        Manager::Pnpm
    } else if lower.contains("/.yarn/") || lower.contains("/yarn/global") {
        Manager::Yarn
    } else if lower.contains("/node_modules/") {
        Manager::Npm
    } else if lower.contains("/cellar/") || lower.contains("/homebrew/") {
        Manager::Homebrew
    } else {
        Manager::Source
    }
}

fn command_for(manager: Manager) -> Option<(&'static str, Vec<String>)> {
    let spec = format!("{PACKAGE}@latest");
    match manager {
        Manager::Npm => Some(("npm", vec!["install".into(), "-g".into(), spec])),
        Manager::Bun => Some(("bun", vec!["add".into(), "-g".into(), spec])),
        Manager::Pnpm => Some(("pnpm", vec!["add".into(), "-g".into(), spec])),
        Manager::Yarn => Some(("yarn", vec!["global".into(), "add".into(), spec])),
        Manager::Homebrew | Manager::Source => None,
    }
}

async fn latest() -> Option<String> {
    let client = bubble_core::http_client().ok()?;
    let answer: serde_json::Value = client
        .get(format!("https://registry.npmjs.org/{PACKAGE}/latest"))
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    answer["version"].as_str().map(str::to_owned)
}

pub async fn run(check_only: bool) -> i32 {
    let current = version();
    println!("Bubble v{current}");
    println!("Checking npm for the latest version…");
    let Some(latest) = latest().await else {
        eprintln!("Could not reach the npm registry. Check your connection and try again.");
        return 1;
    };
    if compare(&latest, current).is_le() {
        println!("You're already on the latest version (v{current}).");
        return 0;
    }
    println!("Update available: v{current} → v{latest}");
    if check_only {
        println!("Run `bubble update` to install it.");
        return 0;
    }
    let path = std::env::current_exe().and_then(|p| p.canonicalize()).unwrap_or_default();
    let manager = manager_of(&path);
    let Some((program, args)) = command_for(manager) else {
        match manager {
            Manager::Homebrew => eprintln!("Bubble was installed via Homebrew. Update it with:\n  brew upgrade bubble"),
            _ => eprintln!(
                "This looks like a build from source ({}), not a global install.\nUpdate it with:\n  git pull && cargo build --release",
                path.display()
            ),
        }
        return 1;
    };
    println!("\nUpdating via {program}…\n");
    let status = tokio::process::Command::new(program).args(&args).status().await;
    match status {
        Ok(status) if status.success() => {
            println!("\n✓ Updated to v{latest}. Restart bubble to use the new version.");
            0
        }
        other => {
            let code = other.ok().and_then(|s| s.code()).unwrap_or(1);
            eprintln!(
                "\nUpdate failed (exit {code}). Try running it manually:\n  {program} {}\nIf this is a permissions error, you may need elevated privileges or to fix your global install prefix.",
                args.join(" ")
            );
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_installs_are_told_apart() {
        assert!(compare("1.0.0", "0.0.62").is_gt());
        assert!(compare("1.2.10", "1.2.9").is_gt());
        assert!(compare("1.0.0-native.1", "1.0.0").is_lt());
        assert!(compare("v1.0.0", "1.0").is_eq());
        assert_eq!(manager_of(Path::new("/opt/homebrew/lib/node_modules/@bubblebrain-ai/bubble-darwin-arm64/bin/bubble")), Manager::Npm);
        assert_eq!(manager_of(Path::new("/Users/me/.bun/install/global/node_modules/x/bin/bubble")), Manager::Bun);
        assert_eq!(manager_of(Path::new("/opt/homebrew/Cellar/bubble/1.0.0/bin/bubble")), Manager::Homebrew);
        assert_eq!(manager_of(Path::new("/Users/me/bb/my-coding-agent/native/target/release/bubble")), Manager::Source);
    }
}
