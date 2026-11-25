//! Feature-branch rollup (spec §4), the opt-in `[velocity] pr_source = "all-merged"` mode.
//!
//! Some teams build a feature on a long-lived feature branch, stack several sub-PRs **into that
//! branch**, then squash-merge the whole branch to the measured branch as one commit. The default
//! `branch-reachable` grouping then sees only that squash: one work package, dated on the merge day,
//! attributed to the squash author — months of work on one date.
//!
//! This module splits such an **umbrella** squash back into the feature-branch PRs stacked into it,
//! using data pmx already fetches (each PR's base branch and its own commit SHAs/dates). The
//! umbrella's score is **apportioned** across its sub-PRs by commit count — points are conserved, so
//! nothing is double-counted — and each sub-PR is re-dated to its own last commit. This is a
//! first-cut heuristic (see issue #1): per-person attribution of the split packages and re-scoring
//! each sub-PR's own diff are follow-ups.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use pm_metrics::velocity::Package;

/// What the rollup needs about one merged PR. Built from `MergedPr` in [`crate::velocity::PrIndex`].
#[derive(Clone, Debug)]
pub struct PrFacts {
    /// `<repo>#<n>` / `<repo>!<n>` — the form `Package.pr` and `commit_refs` use.
    pub id: String,
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
/// non-long-lived-base PRs (its sub-PRs). A sub-PR is claimed by at most one umbrella (the first,
/// by input order), so overlapping stacks don't double-count.
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
            if si == ui || claimed[si] || long_lived(s) || s.own_shas.is_empty() {
                continue;
            }
            // A proper, non-trivial subset: all of the sub-PR's commits are in the umbrella.
            if s.own_shas.is_subset(&u.own_shas) && s.own_shas.len() < u.own_shas.len() {
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
/// sub-PR's last commit and carrying an apportioned share of the umbrella's commits / added lines /
/// points. Points and added lines are conserved. Unscored or unmatched umbrellas are left untouched.
pub fn apply(mut rows: Vec<Package>, facts: &[PrFacts]) -> Vec<Package> {
    let umbrellas = detect(facts);
    if umbrellas.is_empty() {
        return rows;
    }
    let mut drop = BTreeSet::new();
    let mut add: Vec<Package> = Vec::new();
    for u in &umbrellas {
        let uf = &facts[u.umbrella];
        // The umbrella's package is the one that holds its landed (squash) commit.
        let landed: BTreeSet<&str> = uf.landed.iter().map(String::as_str).collect();
        let Some(pi) = rows
            .iter()
            .position(|p| !drop.contains(&p_key(p)) && p.commit_refs.iter().any(|c| landed.contains(c.sha.as_str())))
        else {
            continue;
        };
        let parent = &rows[pi];
        // Only split a scored umbrella; an unscored one would hide its sub-PRs' weeks.
        let Some(points) = parent.points else {
            continue;
        };
        let subs: Vec<&PrFacts> = u.subs.iter().map(|&si| &facts[si]).collect();
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
            // Per-person attribution of a split package is a follow-up (issue #1): the umbrella's
            // squash author would be wrong, so leave it unattributed rather than mislabel it.
            pkg.authors = Default::default();
            pkg.author = None;
            pkg.commit_refs = Vec::new();
            add.push(pkg);
        }
        drop.insert(p_key(parent));
    }
    rows.retain(|p| !drop.contains(&p_key(p)));
    rows.extend(add);
    rows
}

/// A stable key for a package within one `rows` list (id + date are unique per package here).
fn p_key(p: &Package) -> (String, NaiveDate) {
    (p.id.clone(), p.date)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(id: &str, base: &str, shas: &[&str], add: Option<u64>, last: &str, n: u32) -> PrFacts {
        PrFacts {
            id: id.into(),
            base: Some(base.into()),
            own_shas: shas.iter().map(|s| s.to_string()).collect(),
            landed: vec![format!("{id}-landed")],
            additions: add,
            last_date: Some(last.parse().unwrap()),
            commit_count: n,
        }
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
        let out = apply(rows, &f);
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
        assert_eq!(apply(rows.clone(), &f), rows);
        // A plain main PR with no sub-PRs is untouched.
        let f2 = vec![facts("r#80", "main", &["z"], Some(10), "2026-09-01", 1)];
        let rows2 = vec![pkg("r#80", "2026-09-01", Some(2), "r#80-landed")];
        assert_eq!(apply(rows2.clone(), &f2), rows2);
    }
}
