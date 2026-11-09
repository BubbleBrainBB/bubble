//! GitHub through the `gh` CLI, signed in as the person: what a pull request
//! or an issue looks like now, what changed since the last look, what the
//! notifications say, and the few writes an agent may make.
//!
//! Everything that reads JSON is a plain function, tested on fixtures; only
//! [`Gh`] runs commands. `BUBBLE_GH` names another program to run instead
//! of `gh` (tests use a script).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(30);

/// `owner/name` from a git remote URL on github.com.
pub fn repo_from_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| format!("{owner}/{name}"))
}

/// What kind of thing a link points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Issue,
    PullRequest,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Issue => "github_issue",
            Kind::PullRequest => "github_pr",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "github_issue" => Some(Kind::Issue),
            "github_pr" => Some(Kind::PullRequest),
            _ => None,
        }
    }

    /// How a person names it: "issue #7", "PR #12".
    pub fn label(self, number: u64) -> String {
        match self {
            Kind::Issue => format!("issue #{number}"),
            Kind::PullRequest => format!("PR #{number}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub body: String,
    /// Written by the signed-in person (or by Bubble on their behalf).
    pub mine: bool,
    pub bot: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub author: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED…
    pub state: String,
    pub body: String,
    pub mine: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    /// Upper case as GitHub gives it; empty while it runs.
    pub conclusion: String,
}

/// How the checks of a commit came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Checks {
    None,
    Running,
    Passed,
    Failed,
}

/// A pull request or an issue as it is now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub title: String,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    pub url: String,
    #[serde(default)]
    pub head_sha: String,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub reviews: Vec<Review>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

impl Snapshot {
    pub fn checks_state(&self) -> Checks {
        if self.checks.is_empty() {
            return Checks::None;
        }
        if self.checks.iter().any(|c| c.conclusion.is_empty()) {
            return Checks::Running;
        }
        const FAILED: &[&str] = &["FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "ERROR", "STARTUP_FAILURE"];
        if self.checks.iter().any(|c| FAILED.contains(&c.conclusion.as_str())) { Checks::Failed } else { Checks::Passed }
    }

    pub fn failed_checks(&self) -> Vec<String> {
        const FAILED: &[&str] = &["FAILURE", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "ERROR", "STARTUP_FAILURE"];
        self.checks.iter().filter(|c| FAILED.contains(&c.conclusion.as_str())).map(|c| c.name.clone()).collect()
    }
}

fn author_of(value: &Value) -> (String, bool) {
    let login = value["author"]["login"].as_str().unwrap_or_default().to_owned();
    let bot = value["author"]["is_bot"].as_bool().unwrap_or(false) || login.ends_with("[bot]");
    (login, bot)
}

/// Reads `gh pr view --json` or `gh issue view --json` output.
pub fn parse_snapshot(json: &str) -> Result<Snapshot> {
    let value: Value = serde_json::from_str(json).context("gh gave JSON that cannot be read")?;
    let comments = value["comments"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|c| {
                    let (author, bot) = author_of(c);
                    Comment {
                        id: c["id"].as_str().unwrap_or_default().to_owned(),
                        author,
                        body: c["body"].as_str().unwrap_or_default().to_owned(),
                        mine: c["viewerDidAuthor"].as_bool().unwrap_or(false),
                        bot,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let reviews = value["reviews"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|r| {
                    let (author, _) = author_of(r);
                    Review {
                        id: r["id"].as_str().unwrap_or_default().to_owned(),
                        author,
                        state: r["state"].as_str().unwrap_or_default().to_owned(),
                        body: r["body"].as_str().unwrap_or_default().to_owned(),
                        mine: r["viewerDidAuthor"].as_bool().unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let checks = value["statusCheckRollup"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|c| {
                    // Check runs have `conclusion`; commit statuses have `state`.
                    let conclusion = c["conclusion"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| c["state"].as_str().filter(|s| *s != "PENDING" && *s != "EXPECTED"))
                        .unwrap_or_default()
                        .to_owned();
                    Check {
                        name: c["name"].as_str().or_else(|| c["context"].as_str()).unwrap_or_default().to_owned(),
                        conclusion,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Snapshot {
        title: value["title"].as_str().unwrap_or_default().to_owned(),
        state: value["state"].as_str().unwrap_or_default().to_owned(),
        url: value["url"].as_str().unwrap_or_default().to_owned(),
        head_sha: value["headRefOid"].as_str().unwrap_or_default().to_owned(),
        comments,
        reviews,
        checks,
    })
}

/// What changed on a pull request or an issue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    Comment(Comment),
    Review(Review),
    ChecksFailed { sha: String, names: Vec<String> },
    ChecksPassed { sha: String },
    Merged,
    Closed,
    Reopened,
}

impl Change {
    /// A key that is the same each time this change is seen.
    pub fn dedupe_key(&self, repo: &str, number: u64) -> String {
        match self {
            Change::Comment(c) => format!("gh:comment:{}", c.id),
            Change::Review(r) => format!("gh:review:{}", r.id),
            Change::ChecksFailed { sha, .. } => format!("gh:checks:{repo}#{number}:{sha}:failed"),
            Change::ChecksPassed { sha } => format!("gh:checks:{repo}#{number}:{sha}:passed"),
            Change::Merged => format!("gh:merged:{repo}#{number}"),
            Change::Closed => format!("gh:closed:{repo}#{number}"),
            Change::Reopened => format!("gh:reopened:{repo}#{number}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Change::Comment(_) => "comment",
            Change::Review(_) => "review",
            Change::ChecksFailed { .. } => "checks_failed",
            Change::ChecksPassed { .. } => "checks_passed",
            Change::Merged => "merged",
            Change::Closed => "closed",
            Change::Reopened => "reopened",
        }
    }

    /// Who caused it, when someone did.
    pub fn actor(&self) -> Option<&str> {
        match self {
            Change::Comment(c) => Some(&c.author),
            Change::Review(r) => Some(&r.author),
            _ => None,
        }
    }

    /// Made by the person themselves (or a bot): seen, but nothing to act on.
    pub fn is_own_or_bot(&self) -> bool {
        match self {
            Change::Comment(c) => c.mine || c.bot,
            Change::Review(r) => r.mine,
            _ => false,
        }
    }
}

/// The changes between two looks. The first look (`before` is `None`) is
/// a baseline: what was already there is not news.
pub fn changes(before: Option<&Snapshot>, now: &Snapshot) -> Vec<Change> {
    let Some(before) = before else { return Vec::new() };
    let mut changes = Vec::new();
    for comment in &now.comments {
        if !before.comments.iter().any(|c| c.id == comment.id) {
            changes.push(Change::Comment(comment.clone()));
        }
    }
    for review in &now.reviews {
        // Reviews with neither a verdict nor words carry nothing new.
        let empty = review.state == "COMMENTED" && review.body.trim().is_empty();
        if !empty && !before.reviews.iter().any(|r| r.id == review.id) {
            changes.push(Change::Review(review.clone()));
        }
    }
    let (was, is) = (before.checks_state(), now.checks_state());
    let new_commit = before.head_sha != now.head_sha;
    if is == Checks::Failed && (was != Checks::Failed || new_commit) {
        changes.push(Change::ChecksFailed { sha: now.head_sha.clone(), names: now.failed_checks() });
    }
    if is == Checks::Passed && (was != Checks::Passed || new_commit) {
        changes.push(Change::ChecksPassed { sha: now.head_sha.clone() });
    }
    if before.state != now.state {
        match now.state.as_str() {
            "MERGED" => changes.push(Change::Merged),
            "CLOSED" => changes.push(Change::Closed),
            "OPEN" => changes.push(Change::Reopened),
            _ => {}
        }
    }
    changes
}

/// One thread from `GET /notifications`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub thread_id: String,
    pub reason: String,
    pub updated_at: String,
    pub repo: String,
    pub kind: Kind,
    pub number: u64,
    pub title: String,
}

impl Notification {
    pub fn dedupe_key(&self) -> String {
        format!("gh:thread:{}:{}", self.thread_id, self.updated_at)
    }
}

/// Reads the notifications API's answer; threads about anything but issues
/// and pull requests are left out.
pub fn parse_notifications(json: &str) -> Result<Vec<Notification>> {
    let value: Value = serde_json::from_str(json).context("the notifications cannot be read")?;
    let items = value.as_array().ok_or_else(|| anyhow!("the notifications are not a list"))?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let kind = match item["subject"]["type"].as_str()? {
                "Issue" => Kind::Issue,
                "PullRequest" => Kind::PullRequest,
                _ => return None,
            };
            let number = item["subject"]["url"].as_str()?.rsplit('/').next()?.parse().ok()?;
            Some(Notification {
                thread_id: item["id"].as_str()?.to_owned(),
                reason: item["reason"].as_str().unwrap_or_default().to_owned(),
                updated_at: item["updated_at"].as_str().unwrap_or_default().to_owned(),
                repo: item["repository"]["full_name"].as_str()?.to_owned(),
                kind,
                number,
                title: item["subject"]["title"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect())
}

/// The number at the end of a pull request or issue URL.
pub fn number_in_url(url: &str) -> Option<u64> {
    url.trim().trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

/// The `gh` CLI, signed in as the person.
#[derive(Clone, Debug)]
pub struct Gh {
    program: PathBuf,
}

impl Gh {
    /// Finds `gh`: `BUBBLE_GH`, else the login shell's PATH (an app opened
    /// from the Finder has a bare one), else the usual places.
    pub fn locate() -> Option<Self> {
        if let Some(program) = std::env::var_os("BUBBLE_GH").filter(|p| !p.is_empty()) {
            return Some(Gh { program: PathBuf::from(program) });
        }
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
        if let Ok(output) = std::process::Command::new(shell).args(["-lc", "command -v gh"]).output() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if output.status.success() && !path.is_empty() {
                return Some(Gh { program: PathBuf::from(path) });
            }
        }
        ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .map(|program| Gh { program })
    }

    pub fn at(program: impl Into<PathBuf>) -> Self {
        Gh { program: program.into() }
    }

    /// Runs `gh` with `args` in `cwd`; its standard output, or an error with
    /// what it printed.
    pub async fn run(&self, args: &[&str], cwd: Option<&Path>) -> Result<String> {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).env("GH_PROMPT_DISABLED", "1").env("NO_COLOR", "1").kill_on_drop(true);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let output = tokio::time::timeout(TIMEOUT, command.output())
            .await
            .map_err(|_| anyhow!("gh {} took longer than {}s", args.first().unwrap_or(&""), TIMEOUT.as_secs()))?
            .with_context(|| format!("running {}", self.program.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            if stderr.contains("auth login") || stderr.contains("401") {
                bail!("GitHub CLI is not signed in; run `gh auth login`.");
            }
            bail!("gh {}: {}", args.join(" "), if stderr.is_empty() { "failed".to_owned() } else { stderr });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub async fn login(&self) -> Result<String> {
        Ok(self.run(&["api", "user", "--jq", ".login"], None).await?.trim().to_owned())
    }

    pub async fn snapshot(&self, kind: Kind, repo: &str, number: u64) -> Result<Snapshot> {
        let number = number.to_string();
        let json = match kind {
            Kind::PullRequest => {
                self.run(
                    &[
                        "pr", "view", &number, "--repo", repo, "--json",
                        "title,state,url,headRefOid,comments,reviews,statusCheckRollup",
                    ],
                    None,
                )
                .await?
            }
            Kind::Issue => self.run(&["issue", "view", &number, "--repo", repo, "--json", "title,state,url,comments"], None).await?,
        };
        parse_snapshot(&json)
    }

    /// Unread notifications updated since `since` (RFC 3339).
    pub async fn notifications(&self, since: Option<&str>) -> Result<Vec<Notification>> {
        let mut path = "notifications?per_page=50".to_owned();
        if let Some(since) = since {
            path.push_str(&format!("&since={since}"));
        }
        parse_notifications(&self.run(&["api", &path], None).await?)
    }

    /// Comments on an issue or a pull request.
    pub async fn comment(&self, repo: &str, number: u64, body: &str) -> Result<String> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        let body_field = format!("body={body}");
        let out = self.run(&["api", &path, "-X", "POST", "-f", &body_field, "--jq", ".html_url"], None).await?;
        Ok(out.trim().to_owned())
    }

    pub async fn close_issue(&self, repo: &str, number: u64) -> Result<()> {
        self.run(&["issue", "close", &number.to_string(), "--repo", repo], None).await.map(|_| ())
    }

    /// Opens a pull request from the folder's current branch; its URL.
    pub async fn open_pr(&self, cwd: &Path, title: &str, body: &str, base: Option<&str>) -> Result<String> {
        let mut args = vec!["pr", "create", "--title", title, "--body", body];
        if let Some(base) = base {
            args.extend(["--base", base]);
        }
        Ok(self.run(&args, Some(cwd)).await?.trim().lines().last().unwrap_or_default().to_owned())
    }
}

/// The GitHub repository a folder's `origin` remote points at.
pub fn repo_of_folder(folder: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(folder)
        .output()
        .ok()?;
    output.status.success().then(|| repo_from_remote(&String::from_utf8_lossy(&output.stdout))).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str = r#"{
      "title": "Fix login", "state": "OPEN", "url": "https://github.com/acme/demo/pull/12",
      "headRefOid": "abc",
      "comments": [
        {"id": "IC_1", "author": {"login": "lin"}, "body": "Looks close", "viewerDidAuthor": false},
        {"id": "IC_2", "author": {"login": "me"}, "body": "Thanks", "viewerDidAuthor": true}
      ],
      "reviews": [{"id": "PRR_1", "author": {"login": "lin"}, "state": "CHANGES_REQUESTED", "body": "Handle empty input", "viewerDidAuthor": false}],
      "statusCheckRollup": [
        {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE"},
        {"__typename": "StatusContext", "context": "lint", "state": "SUCCESS"}
      ]
    }"#;

    #[test]
    fn remotes_name_their_repository() {
        assert_eq!(repo_from_remote("https://github.com/acme/demo.git").as_deref(), Some("acme/demo"));
        assert_eq!(repo_from_remote("git@github.com:acme/demo.git\n").as_deref(), Some("acme/demo"));
        assert_eq!(repo_from_remote("https://gitlab.com/acme/demo"), None);
        assert_eq!(repo_from_remote("https://github.com/acme"), None);
    }

    #[test]
    fn a_pull_request_reads_with_its_checks() {
        let pr = parse_snapshot(PR).unwrap();
        assert_eq!(pr.comments.len(), 2);
        assert!(pr.comments[1].mine);
        assert_eq!(pr.reviews[0].state, "CHANGES_REQUESTED");
        assert_eq!(pr.checks[1], Check { name: "lint".into(), conclusion: "SUCCESS".into() });
        assert_eq!(pr.checks_state(), Checks::Failed);
        assert_eq!(pr.failed_checks(), ["test"]);
    }

    #[test]
    fn the_first_look_is_a_baseline() {
        assert!(changes(None, &parse_snapshot(PR).unwrap()).is_empty());
    }

    #[test]
    fn new_comments_reviews_and_states_are_changes() {
        let mut before = parse_snapshot(PR).unwrap();
        before.comments.truncate(1);
        before.reviews.clear();
        before.checks[0].conclusion = String::new();
        let now = parse_snapshot(PR).unwrap();
        let found = changes(Some(&before), &now);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(matches!(&found[0], Change::Comment(c) if c.mine));
        assert!(found[0].is_own_or_bot());
        assert!(matches!(&found[1], Change::Review(r) if r.body == "Handle empty input"));
        assert!(matches!(&found[2], Change::ChecksFailed { names, .. } if names == &["test"]));

        let mut merged = now.clone();
        merged.state = "MERGED".into();
        assert_eq!(changes(Some(&now), &merged), [Change::Merged]);
        assert!(changes(Some(&now), &now).is_empty(), "nothing new, nothing reported");
    }

    #[test]
    fn checks_that_pass_after_failing_are_reported_once() {
        let failing = parse_snapshot(PR).unwrap();
        let mut fixed = failing.clone();
        fixed.head_sha = "def".into();
        fixed.checks[0].conclusion = "SUCCESS".into();
        assert_eq!(changes(Some(&failing), &fixed), [Change::ChecksPassed { sha: "def".into() }]);
        assert!(changes(Some(&fixed), &fixed).is_empty());
    }

    #[test]
    fn notifications_keep_issues_and_pull_requests() {
        let json = r#"[
          {"id": "101", "reason": "assign", "updated_at": "2026-11-08T10:00:00Z",
           "repository": {"full_name": "acme/demo"},
           "subject": {"type": "Issue", "title": "Login breaks", "url": "https://api.github.com/repos/acme/demo/issues/7"}},
          {"id": "102", "reason": "subscribed", "updated_at": "2026-11-08T10:00:00Z",
           "repository": {"full_name": "acme/demo"},
           "subject": {"type": "Release", "title": "v1", "url": "https://api.github.com/repos/acme/demo/releases/1"}}
        ]"#;
        let found = parse_notifications(json).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::Issue);
        assert_eq!(found[0].number, 7);
        assert_eq!(found[0].dedupe_key(), "gh:thread:101:2026-11-08T10:00:00Z");
    }

    #[tokio::test]
    async fn gh_failures_carry_what_it_printed() {
        let dir = std::env::temp_dir().join(format!("bubble-gh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("gh");
        std::fs::write(&script, "#!/bin/sh\necho \"HTTP 404: Not Found\" >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let error = Gh::at(&script).snapshot(Kind::Issue, "acme/demo", 7).await.unwrap_err();
        assert!(error.to_string().contains("404"), "{error}");
    }
}
