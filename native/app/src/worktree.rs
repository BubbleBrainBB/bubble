//! Two bots in one repository: the second works in a checkout of its own
//! (a git worktree, on a branch of its own), so neither changes files
//! under the other. A bot is a role and goes where the work is, so this
//! happens whenever two pieces of work in one project run at once.

use std::path::{Path, PathBuf};
use std::process::Command;

use bubble_core::bubble_home;

use crate::state::AppState;

/// Where a bot's turn works, and what it is told about that when it is
/// not the project folder itself.
pub struct Place {
    pub folder: PathBuf,
    pub note: Option<String>,
}

fn place_key(bot_id: &str) -> String {
    format!("bot_place:{bot_id}")
}

/// A bot's checkout of a project, as "project\ncheckout\nbranch".
fn checkout_key(bot_id: &str) -> String {
    format!("bot_checkout:{bot_id}")
}

fn git(folder: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(folder).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `folder` holds changes that are not committed.
pub fn dirty(folder: &Path) -> bool {
    git(folder, &["status", "--porcelain"]).is_none_or(|changes| !changes.is_empty())
}

/// Makes a checkout of `project` at `at`, on the new branch `branch` from
/// what the project has checked out now.
pub fn add(project: &Path, at: &Path, branch: &str) -> bool {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    git(project, &["worktree", "add", "-b", branch, &at.to_string_lossy(), "HEAD"]).is_some()
}

/// Takes a checkout away; its branch, and what was committed on it, stay.
pub fn remove(project: &Path, at: &Path) -> bool {
    git(project, &["worktree", "remove", &at.to_string_lossy()]).is_some()
}

/// A branch name's part for `name`: its letters and digits, lowercased.
fn slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { "bot".to_owned() } else { slug }
}

/// The checkout `bot_id` has of `project`, while it is there.
fn checkout_of(app: AppState, bot_id: &str, project: &Path) -> Option<(PathBuf, String)> {
    let saved = app.with_store(|s| s.setting(&checkout_key(bot_id))).flatten()?;
    let mut parts = saved.splitn(3, '\n');
    let (of, at, branch) = (parts.next()?, parts.next()?, parts.next()?);
    (Path::new(of) == project && Path::new(at).is_dir()).then(|| (PathBuf::from(at), branch.to_owned()))
}

fn told(project: &Path, at: &Path, branch: &str) -> String {
    format!(
        "Another bot is working in {} right now, so you work in a checkout of your own of the same repository, at {} on the branch \
         {branch}. It starts from what that folder had checked out. Commit your work on this branch; it reaches the main folder by a \
         pull request or by the person merging the branch, so say which branch it is on when you report.",
        project.display(),
        at.display()
    )
}

/// Where `bot_id` works for a turn in `project`: the folder itself, or a
/// checkout of its own while another bot is at work there. `busy_elsewhere`
/// says which other bots are in a turn now.
pub fn place_for(app: AppState, bot_id: &str, name: &str, project: &Path, busy: &[String]) -> Place {
    // Another bot's turn runs in the project folder itself.
    let occupied = busy.iter().filter(|other| other.as_str() != bot_id).any(|other| {
        app.with_store(|s| s.setting(&place_key(other))).flatten().is_some_and(|place| Path::new(&place) == project)
    });
    let mine = checkout_of(app, bot_id, project);
    let place = match (mine, occupied) {
        (Some((at, branch)), true) => Place { note: Some(told(project, &at, &branch)), folder: at },
        // The folder is free again: back to it, unless work is left in the checkout.
        (Some((at, branch)), false) if dirty(&at) => Place { note: Some(told(project, &at, &branch)), folder: at },
        (Some((at, _)), false) => {
            remove(project, &at);
            app.with_store(|s| s.set_setting(&checkout_key(bot_id), None));
            Place { folder: project.to_owned(), note: None }
        }
        (None, true) => {
            let short: String = bot_id.chars().rev().take(8).collect::<String>().chars().rev().collect();
            let stamp = format!("{:x}", bubble_store::now_ms() / 1000 % 0x10000);
            let branch = format!("bubble/{}-{stamp}", slug(name));
            let at = bubble_home()
                .join("native")
                .join("worktrees")
                .join(project.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into()))
                .join(format!("{short}-{stamp}"));
            if add(project, &at, &branch) {
                let saved = format!("{}\n{}\n{branch}", project.display(), at.display());
                app.with_store(|s| s.set_setting(&checkout_key(bot_id), Some(&saved)));
                Place { note: Some(told(project, &at, &branch)), folder: at }
            } else {
                // Not a git repository, or git said no: the folder is shared.
                log::warn!("no checkout of {} could be made for {name}; it shares the folder", project.display());
                Place { folder: project.to_owned(), note: None }
            }
        }
        (None, false) => Place { folder: project.to_owned(), note: None },
    };
    app.with_store(|s| s.set_setting(&place_key(bot_id), Some(&place.folder.to_string_lossy())));
    place
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(folder: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(folder).args(args).status().unwrap().success(), "git {args:?}");
    }

    #[test]
    fn a_checkout_of_its_own_is_made_used_and_taken_away() {
        let root = std::env::temp_dir().join(format!("bubble-worktree-{}", std::process::id()));
        let project = root.join("demo");
        std::fs::create_dir_all(&project).unwrap();
        run(&project, &["init", "-q"]);
        std::fs::write(project.join("a.txt"), "one").unwrap();
        run(&project, &["add", "."]);
        run(&project, &["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "first"]);

        let at = root.join("checkouts").join("dev");
        assert!(add(&project, &at, "bubble/dev-1"));
        assert_eq!(std::fs::read_to_string(at.join("a.txt")).unwrap(), "one");
        assert!(!dirty(&at));
        std::fs::write(at.join("b.txt"), "two").unwrap();
        assert!(dirty(&at), "a new file is work left in it");
        assert!(!project.join("b.txt").exists(), "the project folder is untouched");
        // Work left in it keeps it.
        assert!(!remove(&project, &at));
        std::fs::remove_file(at.join("b.txt")).unwrap();
        assert!(remove(&project, &at));
        assert!(!at.exists());
        // A folder that is no repository gets no checkout.
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!add(&plain, &root.join("checkouts").join("x"), "bubble/x"));
        assert_eq!(slug("开发 Dev 2"), "dev-2");
        assert_eq!(slug("开发"), "bot");
        std::fs::remove_dir_all(root).ok();
    }
}
