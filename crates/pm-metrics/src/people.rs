//! `private/people.json` (spec §10.9, plan D64): the per-person facts behind a separate people
//! report. Lead only, like `leads.json`: real names and emails, never in `project.json` or the fleet.
//! A file contract, so the report can live in its own repo without pmx's crates.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use pm_classify::Role;
use serde::{Deserialize, Serialize};

pub const PEOPLE_SCHEMA: &str = "pmx.people/1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeopleFile {
    pub schema: String,
    pub project: String,
    pub range_start: NaiveDate,
    pub as_of: NaiveDate,
    pub generated_at: String,
    pub breadth_roles: Vec<Role>,
    pub ai_attribution: bool,
    pub repos: Vec<PeopleRepo>,
    /// Commits by included authors (spec §1.3) from `range_start` to `as_of`, newest first.
    pub commits: Vec<CommitFact>,
    /// Work packages (spec §4), newest first: Velocity's when it ran, else grouped the same way
    /// without scores. With Velocity, only from `[velocity] since` on.
    pub packages: Vec<PackageFact>,
    /// How the packages were scored; `None` = Velocity not measured (no points).
    pub velocity: Option<PeopleVelocity>,
    /// Merged PRs/MRs by included authors; `None` = peer review not measured.
    pub prs: Option<Vec<PrFact>>,
}

/// What a link into a repo needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeopleRepo {
    pub name: String,
    /// A stack role or `per-file`.
    pub role: String,
    /// The measured tip: commit lists link to it, so they show what was measured.
    pub sha: String,
    /// `github` or `gitlab`; `None` when the project has no code host.
    pub host: Option<String>,
    /// The repo's web page (`https://host/group/name`), from its URL or `origin` remote.
    pub web_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommitFact {
    pub repo: String,
    pub sha: String,
    /// Author date (spec §1.1).
    pub date: NaiveDate,
    /// Canonical person (`[people]`).
    pub person: String,
    /// The author email as recorded in the commit.
    pub email: String,
    pub subject: String,
    /// AI-assisted (spec §6).
    pub ai: bool,
    /// Added lines in source files.
    pub added: u64,
    /// Added + deleted lines in source files.
    pub size: u64,
    /// Touches a prod file, a test file, a doc file (spec §1.4).
    pub prod: bool,
    pub test: bool,
    pub doc: bool,
    /// Role → technology → added source lines (spec §1.5).
    pub lines: BTreeMap<Role, BTreeMap<String, u64>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeopleVelocity {
    pub rubric: String,
    pub classifiers: Vec<String>,
    pub since: NaiveDate,
}

/// A work package (spec §4) with what a link to it needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackageFact {
    /// Ticket id, `<repo>#<n>` / `<repo>!<n>`, or `<person>~<ISO week>` (real names here).
    pub id: String,
    /// `ticket`, `pr` or `author-week`.
    pub kind: String,
    pub date: NaiveDate,
    pub description: String,
    pub repos: Vec<String>,
    /// Person → commits.
    pub authors: BTreeMap<String, u32>,
    /// Oldest first.
    pub commits: Vec<CommitRef>,
    pub added: u64,
    pub points: Option<u32>,
    pub size: Option<String>,
    pub axes: Option<[u8; 6]>,
    pub tests: bool,
    /// Verification-debt points (spec §4).
    pub debt: u32,
    /// The PR/MR of a `pr` package.
    pub pr: Option<PrRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRef {
    pub repo: String,
    pub sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub repo: String,
    /// Set when the MR lives in another project path (a moved repo, plan D38).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub number: u64,
}

/// One merged PR/MR (spec §8), resolved to people.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrFact {
    pub repo: String,
    pub number: u64,
    /// Canonical person; `None` for a deleted account.
    pub author: Option<String>,
    pub created_at: String,
    pub merged_at: String,
    /// Merge date in the host's recorded offset (spec §1.1).
    pub merged: NaiveDate,
    pub cycle_ms: u64,
    pub first_review_ms: Option<u64>,
    /// People with a review on it (non-author, non-bot).
    pub reviewers: Vec<String>,
    /// Expert reviewers of a non-expert author (spec §8 mentoring).
    pub mentors: Vec<String>,
    pub stack: Option<Role>,
    /// `additions + deletions` (GitHub only).
    pub size: Option<u64>,
}

/// `#123` / `!123` → 123.
pub fn pr_number(id: &str) -> Option<u64> {
    id.trim_start_matches(['#', '!']).parse().ok()
}

/// A Velocity PR package id (`<repo>#<n>`, `<repo>!<n>`, `<repo>:<path>!<n>`) → its PR.
pub fn pr_ref(id: &str) -> Option<PrRef> {
    let at = id.rfind(['#', '!'])?;
    let number = id[at + 1..].parse().ok()?;
    let head = &id[..at];
    let (repo, path) = match head.split_once(':') {
        Some((r, p)) => (r, Some(p.to_string())),
        None => (head, None),
    };
    Some(PrRef {
        repo: repo.to_string(),
        path,
        number,
    })
}

/// A remote URL (`https://…`, `git@host:group/name.git`, `ssh://git@host/…`) → the repo's web page.
pub fn web_url(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches('/');
    let r = r.strip_suffix(".git").unwrap_or(r);
    let (host, path) = if let Some(rest) = r.strip_prefix("https://").or_else(|| r.strip_prefix("http://")) {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
        rest.split_once('/')?
    } else if let Some(rest) = r.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map_or(rest, |(_, h)| h);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next().unwrap_or(host), path)
    } else {
        let rest = r.split_once('@').map_or(r, |(_, h)| h);
        rest.split_once(':')?
    };
    (!host.is_empty() && !path.is_empty() && !path.starts_with('/')).then(|| format!("https://{host}/{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls() {
        let w = |s| web_url(s);
        assert_eq!(
            w("git@github.com:acme/shop.git").as_deref(),
            Some("https://github.com/acme/shop")
        );
        assert_eq!(
            w("https://github.com/acme/shop.git").as_deref(),
            Some("https://github.com/acme/shop")
        );
        assert_eq!(
            w("https://oauth2:tok@gitlab.example.com/g/sub/app").as_deref(),
            Some("https://gitlab.example.com/g/sub/app")
        );
        assert_eq!(
            w("ssh://git@gitlab.example.com:2222/g/app.git").as_deref(),
            Some("https://gitlab.example.com/g/app")
        );
        assert_eq!(w("/local/path/repo"), None);
    }

    #[test]
    fn pr_refs() {
        assert_eq!(
            pr_ref("api#12"),
            Some(PrRef {
                repo: "api".into(),
                path: None,
                number: 12
            })
        );
        assert_eq!(
            pr_ref("api:old/api!7"),
            Some(PrRef {
                repo: "api".into(),
                path: Some("old/api".into()),
                number: 7
            })
        );
        assert_eq!(pr_ref("PROJ-12"), None);
        assert_eq!(pr_number("!42"), Some(42));
    }
}
