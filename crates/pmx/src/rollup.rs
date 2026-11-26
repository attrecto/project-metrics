//! Feature-branch rollup (spec §4), the opt-in `[velocity] pr_source = "all-merged"` mode.
//!
//! Some teams build a feature on a long-lived feature branch, stack several sub-PRs **into that
//! branch**, then squash-merge the whole branch to the measured branch as one commit. The default
//! `branch-reachable` grouping then sees only that squash: one work package, dated on the merge day,
//! attributed to the squash author — months of work on one date.
//!
//! This module splits such an **umbrella** squash back into the feature-branch PRs stacked into it,
//! using data pmx already fetches (each PR's base branch, its own commits and its landed SHAs). The
//! umbrella's score is **apportioned** across its sub-PRs by commit count — points are conserved, so
//! nothing is double-counted — and each sub-PR is re-dated to its own last commit and attributed to
//! its own commit authors. It only touches a package that is the umbrella squash alone, and only
//! when none of the sub-PRs' work is already measured. Re-scoring each sub-PR's own diff is a
//! follow-up (issue #1).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use pm_codehost::Account;
use pm_config::Identity;
use pm_metrics::people::CommitRef;
use pm_metrics::velocity::Package;

/// What the rollup needs about one merged PR. Built from `MergedPr` in [`crate::velocity::PrIndex`].
#[derive(Clone, Debug)]
pub struct PrFacts {
    /// `<repo>#<n>` / `<repo>!<n>` — the form `Package.pr` and `commit_refs` use.
    pub id: String,
    /// The repo the PR belongs to (`CommitRef.repo`).
    pub repo: String,
    /// The PR's author on the code host: who a sub-PR is attributed to when its commits are not.
    pub author: Option<Account>,
    /// The author email of each of the PR's own commits (in `own_shas`), where the host gave one.
    pub commit_emails: Vec<String>,
    /// The PR's base (target) branch, if the host reported it.
    pub base: Option<String>,
    /// The PR's own commit SHAs: a sub-PR's are a subset of its umbrella's.
    pub own_shas: BTreeSet<String>,
    /// The SHA(s) the PR landed as on its target branch (merge/squash commit).
    pub landed: Vec<String>,
    /// Added lines (GitHub reports this per PR), if known.
    pub additions: Option<u64>,
    /// The latest authored date among the PR's commits — the sub-PR's real date.
    pub last_date: Option<NaiveDate>,
    /// How many commits the PR carried.
    pub commit_count: u32,
}

/// Is `branch` a long-lived integration branch (the kind an umbrella squash lands on)?
pub fn is_long_lived(branch: &str) -> bool {
    let b = branch.strip_prefix("origin/").unwrap_or(branch);
    matches!(b, "main" | "master" | "develop" | "dev" | "staging" | "production") || b.starts_with("release")
}

/// An umbrella PR (long-lived base) and the indices of the feature-branch PRs stacked into it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Umbrella {
    pub umbrella: usize,
    pub subs: Vec<usize>,
}

/// Find umbrellas: a long-lived-base PR whose own commits contain the commits of one or more
/// non-long-lived-base PRs (its sub-PRs), or the commit a sub-PR was squash-merged into the feature
/// branch as. A sub-PR is claimed by at most one umbrella (the first, by input order), so
/// overlapping stacks don't double-count.
pub fn detect(facts: &[PrFacts]) -> Vec<Umbrella> {
    let long_lived = |f: &PrFacts| f.base.as_deref().is_some_and(is_long_lived);
    let mut claimed = vec![false; facts.len()];
    let mut out = Vec::new();
    for (ui, u) in facts.iter().enumerate() {
        if !long_lived(u) || u.own_shas.is_empty() {
            continue;
        }
        let mut subs = Vec::new();
        for (si, s) in facts.iter().enumerate() {
            if si == ui || claimed[si] || long_lived(s) {
                continue;
            }
            // A proper, non-trivial subset: all of the sub-PR's commits are in the umbrella.
            let stacked =
                !s.own_shas.is_empty() && s.own_shas.is_subset(&u.own_shas) && s.own_shas.len() < u.own_shas.len();
            // Squash-merged into the feature branch: the umbrella carries the sub-PR's squash.
            let squashed = s.landed.iter().any(|l| u.own_shas.contains(l));
            if stacked || squashed {
                subs.push(si);
            }
        }
        if !subs.is_empty() {
            subs.iter().for_each(|&si| claimed[si] = true);
            out.push(Umbrella { umbrella: ui, subs });
        }
    }
    out
}

/// Split `total` into `weights.len()` integers proportional to `weights`, summing exactly to
/// `total` (largest-remainder). All-zero weights share equally.
fn apportion(total: u32, weights: &[u32]) -> Vec<u32> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: u64 = weights.iter().map(|&w| w as u64).sum();
    if sum == 0 {
        // Degenerate: spread as evenly as possible.
        let base = total / n as u32;
        let mut r = (total % n as u32) as usize;
        return (0..n)
            .map(|_| {
                base + if r > 0 {
                    r -= 1;
                    1
                } else {
                    0
                }
            })
            .collect();
    }
    let mut floors: Vec<u32> = weights
        .iter()
        .map(|&w| ((w as u64 * total as u64) / sum) as u32)
        .collect();
    let mut left = total - floors.iter().sum::<u32>();
    // Give the leftover to the largest fractional remainders.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let fr = |i: usize| (weights[i] as u64 * total as u64) % sum;
        fr(b).cmp(&fr(a)).then(weights[b].cmp(&weights[a]))
    });
    for &i in &order {
        if left == 0 {
            break;
        }
        floors[i] += 1;
        left -= 1;
    }
    floors
}

/// Replace each umbrella squash package in `rows` with one package per sub-PR, re-dated to the
/// sub-PR's last commit, attributed to its own commit authors and carrying an apportioned share of
/// the umbrella's points (conserved). An umbrella is left as it is when it is unscored, when its
/// package also holds other work (a ticket package, say), or when any sub-PR's commits are already
/// in a package (a merge-commit umbrella brought them in): splitting would then move or count work
/// twice. Returns the rows and the ids of the umbrellas left alone for those last two reasons.
pub fn apply(mut rows: Vec<Package>, facts: &[PrFacts], identity: &Identity) -> (Vec<Package>, Vec<String>) {
    let umbrellas = detect(facts);
    if umbrellas.is_empty() {
        return (rows, Vec::new());
    }
    // Every commit already in some package, before anything is split.
    let measured: BTreeSet<&str> = rows
        .iter()
        .flat_map(|p| p.commit_refs.iter().map(|c| c.sha.as_str()))
        .collect();
    let mut drop = BTreeSet::new();
    let mut add: Vec<Package> = Vec::new();
    let mut kept = Vec::new();
    for u in &umbrellas {
        let uf = &facts[u.umbrella];
        // The umbrella's package is the one that holds its landed (squash) commit.
        let landed: BTreeSet<&str> = uf.landed.iter().map(String::as_str).collect();
        let Some(pi) = rows
            .iter()
            .position(|p| p.commit_refs.iter().any(|c| landed.contains(c.sha.as_str())))
        else {
            continue;
        };
        let parent = &rows[pi];
        // Only split a scored umbrella; an unscored one would hide its sub-PRs' weeks.
        let Some(points) = parent.points else {
            continue;
        };
        let subs: Vec<&PrFacts> = u.subs.iter().map(|&si| &facts[si]).collect();
        // Only the umbrella's own commits: anything else in the package is other work.
        let mixed = drop.contains(&pi)
            || parent
                .commit_refs
                .iter()
                .any(|c| !landed.contains(c.sha.as_str()) && !uf.own_shas.contains(&c.sha));
        let already = subs.iter().any(|s| {
            s.own_shas
                .iter()
                .chain(&s.landed)
                .any(|sha| measured.contains(sha.as_str()))
        });
        if mixed || already {
            kept.push(uf.id.clone());
            continue;
        }
        let weights: Vec<u32> = subs.iter().map(|s| s.commit_count.max(1)).collect();
        let pt_split = apportion(points, &weights);
        let add_split = apportion(parent.added.min(u32::MAX as u64) as u32, &weights);
        for (k, s) in subs.iter().enumerate() {
            let mut pkg = parent.clone();
            pkg.id = s.id.clone();
            pkg.pr = Some(s.id.clone());
            pkg.date = s.last_date.unwrap_or(parent.date);
            pkg.commits = s.commit_count;
            pkg.added = s.additions.unwrap_or(add_split[k] as u64);
            pkg.points = Some(pt_split[k]);
            let authors = authors(s, identity);
            // No one to attribute it to: keep the umbrella's people, so per-person totals hold.
            if !authors.is_empty() {
                pkg.authors = authors;
            }
            pkg.author = None;
            pkg.commit_refs = s
                .own_shas
                .iter()
                .map(|sha| CommitRef {
                    repo: s.repo.clone(),
                    sha: sha.clone(),
                })
                .collect();
            add.push(pkg);
        }
        drop.insert(pi);
    }
    let mut i = 0;
    rows.retain(|_| {
        i += 1;
        !drop.contains(&(i - 1))
    });
    rows.extend(add);
    (rows, kept)
}

/// Commits per person of a sub-PR, resolved like the measured commits (spec §1.3: bots and
/// externals count for no one). A commit whose email is not in `[people]` goes to the PR's author.
fn authors(s: &PrFacts, identity: &Identity) -> BTreeMap<String, u32> {
    let pr_author = s
        .author
        .as_ref()
        .map(|a| identity.resolve_login(&a.login, a.name.as_deref(), a.bot))
        .filter(|a| a.excluded.is_none())
        .map(|a| a.person);
    let mut out = BTreeMap::new();
    for email in &s.commit_emails {
        let a = identity.resolve(email, "");
        if a.excluded.is_some() {
            continue;
        }
        let person = Some(a.person).filter(|p| !p.is_empty()).or_else(|| pr_author.clone());
        if let Some(p) = person {
            *out.entry(p).or_default() += 1;
        }
    }
    if out.is_empty()
        && let Some(p) = pr_author
    {
        out.insert(p, s.commit_count.max(1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(id: &str, base: &str, shas: &[&str], add: Option<u64>, last: &str, n: u32) -> PrFacts {
        PrFacts {
            id: id.into(),
            repo: "r".into(),
            author: None,
            commit_emails: Vec::new(),
            base: Some(base.into()),
            own_shas: shas.iter().map(|s| s.to_string()).collect(),
            landed: vec![format!("{id}-landed")],
            additions: add,
            last_date: Some(last.parse().unwrap()),
            commit_count: n,
        }
    }

    fn no_people() -> Identity {
        Identity::new(&pm_config::People::default())
    }

    fn apply_plain(rows: Vec<Package>, f: &[PrFacts]) -> Vec<Package> {
        apply(rows, f, &no_people()).0
    }

    fn pkg(id: &str, date: &str, points: Option<u32>, landed_sha: &str) -> Package {
        Package {
            id: id.into(),
            date: date.parse().unwrap(),
            repos: vec!["r".into()],
            commits: 1,
            added: 300,
            description: String::new(),
            axes: Some([1, 1, 1, 0, 0, 1]),
            points,
            size: Some("M".into()),
            tests: false,
            truncated: false,
            classifier: Some("c".into()),
            model: Some("m".into()),
            escalated: false,
            low_confidence: false,
            unknown_files: 0,
            author: None,
            authors: Default::default(),
            pr: Some(id.into()),
            commit_refs: vec![pm_metrics::people::CommitRef {
                repo: "r".into(),
                sha: landed_sha.into(),
            }],
        }
    }

    #[test]
    fn detects_umbrella_and_its_subs() {
        let f = vec![
            facts("r#99", "main", &["a", "b", "c", "d"], Some(300), "2026-10-20", 4),
            facts("r#95", "feature/x", &["a", "b"], Some(120), "2026-10-05", 2),
            facts("r#96", "feature/x", &["c"], Some(40), "2026-10-12", 1),
            facts("r#80", "main", &["z"], Some(10), "2026-09-01", 1), // normal main PR, no subs
        ];
        let u = detect(&f);
        assert_eq!(
            u,
            vec![Umbrella {
                umbrella: 0,
                subs: vec![1, 2]
            }]
        );
    }

    #[test]
    fn apportion_conserves_and_splits_by_weight() {
        assert_eq!(apportion(3, &[2, 1]), vec![2, 1]);
        assert_eq!(apportion(10, &[1, 1, 1]).iter().sum::<u32>(), 10);
        assert_eq!(apportion(5, &[0, 0]), vec![3, 2]); // degenerate weights, still conserved
        assert_eq!(apportion(0, &[4, 1]), vec![0, 0]);
    }

    #[test]
    fn splits_umbrella_package_into_subs_conserving_points() {
        let f = vec![
            facts("r#99", "main", &["a", "b", "c"], Some(300), "2026-10-20", 3),
            facts("r#95", "feature/x", &["a", "b"], Some(200), "2026-10-05", 2),
            facts("r#96", "feature/x", &["c"], None, "2026-10-12", 1),
        ];
        // The umbrella's package holds its landed sha "r#99-landed", scored at 6 points / 300 added.
        let rows = vec![pkg("ACTUATE-1", "2026-10-20", Some(6), "r#99-landed")];
        let out = apply_plain(rows, &f);
        // Umbrella replaced by its two sub-PRs.
        let ids: BTreeSet<&str> = out.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["r#95", "r#96"].into_iter().collect());
        // Points conserved.
        assert_eq!(out.iter().map(|p| p.points.unwrap()).sum::<u32>(), 6);
        // Dated at each sub-PR's own last commit.
        let s95 = out.iter().find(|p| p.id == "r#95").unwrap();
        assert_eq!(s95.date, "2026-10-05".parse::<NaiveDate>().unwrap());
        assert_eq!(s95.points, Some(4)); // 2 of 3 commits → 4 of 6 points
        assert_eq!(s95.added, 200); // sub-PR's own additions used when known
        let s96 = out.iter().find(|p| p.id == "r#96").unwrap();
        assert_eq!(s96.date, "2026-10-12".parse::<NaiveDate>().unwrap());
        assert_eq!(s96.points, Some(2));
        assert_eq!(s96.added, 100); // additions unknown → apportioned share of 300
    }

    #[test]
    fn leaves_unscored_or_subless_umbrellas_alone() {
        let f = vec![
            facts("r#99", "main", &["a", "b"], Some(300), "2026-10-20", 2),
            facts("r#95", "feature/x", &["a"], Some(100), "2026-10-05", 1),
        ];
        // Unscored umbrella package: must be left as-is (splitting would blank its sub-PRs' weeks).
        let rows = vec![pkg("r#99", "2026-10-20", None, "r#99-landed")];
        assert_eq!(apply_plain(rows.clone(), &f), rows);
        // A plain main PR with no sub-PRs is untouched.
        let f2 = vec![facts("r#80", "main", &["z"], Some(10), "2026-09-01", 1)];
        let rows2 = vec![pkg("r#80", "2026-09-01", Some(2), "r#80-landed")];
        assert_eq!(apply_plain(rows2.clone(), &f2), rows2);
    }

    #[test]
    fn detects_subs_squashed_into_the_feature_branch() {
        // r#95 and r#96 were squashed into feature/x as their landed SHAs; the umbrella carries those.
        let f = vec![
            facts(
                "r#99",
                "main",
                &["r#95-landed", "r#96-landed"],
                Some(300),
                "2026-10-20",
                2,
            ),
            facts("r#95", "feature/x", &["a", "b"], Some(120), "2026-10-05", 2),
            facts("r#96", "feature/x", &["c"], Some(40), "2026-10-12", 1),
        ];
        assert_eq!(
            detect(&f),
            vec![Umbrella {
                umbrella: 0,
                subs: vec![1, 2]
            }]
        );
    }

    #[test]
    fn split_packages_keep_their_people_and_commits() {
        let mut people = pm_config::People::default();
        people.persons.insert("Ann".into(), vec!["ann@work.example".into()]);
        people.persons.insert("Bob".into(), vec!["bob-gh".into()]);
        let id = Identity::new(&people);
        let mut f = vec![
            facts("r#99", "main", &["a", "b", "c"], Some(300), "2026-10-20", 3),
            facts("r#95", "feature/x", &["a", "b"], Some(200), "2026-10-05", 2),
            facts("r#96", "feature/x", &["c"], None, "2026-10-12", 1),
        ];
        f[1].commit_emails = vec!["ann@work.example".into(), "ann@work.example".into()];
        // r#96's commit email is unknown: it goes to the PR's author.
        f[2].commit_emails = vec!["someone@else.example".into()];
        f[2].author = Some(Account {
            login: "bob-gh".into(),
            name: None,
            bot: false,
        });
        let mut parent = pkg("r#99", "2026-10-20", Some(6), "r#99-landed");
        parent.authors = [("Squasher".to_string(), 1)].into();
        let out = apply(vec![parent], &f, &id).0;
        let by = |i: &str| out.iter().find(|p| p.id == i).unwrap();
        assert_eq!(by("r#95").authors, [("Ann".to_string(), 2)].into());
        assert_eq!(by("r#96").authors, [("Bob".to_string(), 1)].into());
        let shas: Vec<&str> = by("r#95").commit_refs.iter().map(|c| c.sha.as_str()).collect();
        assert_eq!(shas, ["a", "b"]);
        let per =
            pm_metrics::velocity::points_by_person(&out, "2026-01-01".parse().unwrap(), "2026-12-31".parse().unwrap());
        assert_eq!(per.values().sum::<f64>(), 6.0, "every point is someone's");
    }

    #[test]
    fn leaves_a_package_that_holds_other_work_alone() {
        let f = vec![
            facts("r#99", "main", &["a", "b"], Some(300), "2026-10-20", 2),
            facts("r#95", "feature/x", &["a"], Some(100), "2026-10-05", 1),
        ];
        // A ticket package: the umbrella squash plus an unrelated fix.
        let mut ticket = pkg("SHOP-12", "2026-10-20", Some(8), "r#99-landed");
        ticket.commit_refs.push(CommitRef {
            repo: "r".into(),
            sha: "fix".into(),
        });
        let rows = vec![ticket];
        let (out, kept) = apply(rows.clone(), &f, &no_people());
        assert_eq!(out, rows);
        assert_eq!(kept, ["r#99"]);
    }

    #[test]
    fn does_not_count_sub_prs_twice() {
        let f = vec![
            facts("r#99", "main", &["a", "b"], Some(300), "2026-10-20", 2),
            facts("r#95", "feature/x", &["a"], Some(100), "2026-10-05", 1),
        ];
        // r#95's commit is already measured in its own package.
        let rows = vec![
            pkg("r#99", "2026-10-20", Some(6), "r#99-landed"),
            pkg("r#95", "2026-10-05", Some(2), "a"),
        ];
        let (out, kept) = apply(rows.clone(), &f, &no_people());
        assert_eq!(out, rows);
        assert_eq!(kept, ["r#99"]);
    }
}
