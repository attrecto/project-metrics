//! `pmx check --gaps`: what the last collect did not measure, against what the lead accepted in
//! `[[accept]]` (plan D68). A gap is read from `out/project.json`, so it is about the data that
//! would be shared, not about what the config promises.

use std::io::Write;

use pm_config::{Config, GAP_IDS};
use pm_metrics::ProjectFile;

/// One metric family the output goes without.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    /// One of [`GAP_IDS`].
    pub id: &'static str,
    pub label: &'static str,
    /// Why it is missing and what fixes it.
    pub reason: String,
}

/// The latest snapshot value that tells whether a snapshot-based family was measured.
const SNAPSHOT_FAMILIES: &[(&str, &str, &str, &str)] = &[
    ("duplication", "Duplication", "dup_pct", "jscpd"),
    ("complexity", "Complexity", "cx_per_kloc", "scc"),
    (
        "dependency_vulnerabilities",
        "Dependency vulnerabilities and the security score",
        "security_score",
        "osv-scanner",
    ),
    ("secrets", "Live secrets", "secrets_high", "gitleaks"),
    ("sast", "SAST", "sast_high", "semgrep"),
];

fn id(s: &str) -> &'static str {
    GAP_IDS.iter().copied().find(|g| *g == s).expect("a known gap id")
}

/// Every gap in a collected project, in a fixed order.
///
/// `pr_size` is left out on purpose: only GitHub reports it, so on other hosts it is a property
/// of the host, not something a lead can fix or forget.
pub fn gaps(p: &ProjectFile) -> Vec<Gap> {
    let mut out = Vec::new();
    let tool_missing = |g: &str| p.meta.not_measured.iter().any(|x| x == g);

    match &p.velocity {
        None => out.push(Gap {
            id: id("velocity"),
            label: "Velocity",
            reason: "no classifier scored the work packages: add [classifiers] and [velocity] \
                     (`pmx classifier test <name>`)"
                .into(),
        }),
        Some(v) if v.health.packages > 0 && v.health.unscored == v.health.packages => out.push(Gap {
            id: id("velocity"),
            label: "Velocity",
            reason: format!(
                "none of the {} work packages is scored: see collect's warnings \
                 (`pmx classifier test <name>` checks the login)",
                v.health.packages
            ),
        }),
        Some(v) if v.options.fte.is_empty() => out.push(Gap {
            id: id("fte"),
            label: "FTE",
            reason: "the team's FTE is unknown, so Velocity divides by active engineer-days \
                     (approximate): set [fte], or see collect's warnings if it is set"
                .into(),
        }),
        Some(_) => {}
    }

    if p.meta.pull_requests.is_none() {
        out.push(Gap {
            id: id("peer_review"),
            label: "Peer review and PR flow",
            reason: "no pull or merge requests were read: [code_host], a code_host_id per repo and \
                     the token (`pmx check`)"
                .into(),
        });
    }

    let latest = p.snapshots.values().next_back();
    for (gap, label, value, tool) in SNAPSHOT_FAMILIES {
        let measured = latest.is_some_and(|s| s.values.get(*value).copied().flatten().is_some());
        if measured {
            continue;
        }
        let reason = if latest.is_none() {
            "no snapshot was taken: check [snapshots] and that collect ran without --no-snapshots".into()
        } else if tool_missing(gap) {
            format!("{tool} was not found on the machine that collected: `pmx doctor`, `pmx tools install`")
        } else if *gap == "dependency_vulnerabilities" {
            "a lockfile failed to scan in at least one repo at the latest snapshot: see collect's \
             warnings, and name the lockfiles that count in that repo's `security_lockfiles`"
                .into()
        } else {
            format!("{tool} failed for at least one repo at the latest snapshot: see collect's warnings")
        };
        out.push(Gap {
            id: id(gap),
            label,
            reason,
        });
    }
    if tool_missing("hotspots") || (latest.is_some() && p.hotspots.is_empty()) {
        out.push(Gap {
            id: id("hotspots"),
            label: "Hotspots",
            reason: "scc was not found or no repo could be read: `pmx doctor`".into(),
        });
    }
    out
}

/// Print the gaps against `[[accept]]`; returns how many are not accepted.
pub fn report(gaps: &[Gap], config: &Config, w: &mut dyn Write) -> std::io::Result<usize> {
    let accepted = |g: &str| config.accepted_gaps.iter().find(|a| a.gap == g);
    let mut open = 0;
    for g in gaps {
        match accepted(g.id) {
            Some(a) => writeln!(w, "– {} (`{}`): accepted: {}", g.label, g.id, a.reason.trim())?,
            None => {
                open += 1;
                writeln!(w, "✗ {} (`{}`) is not measured: {}", g.label, g.id, g.reason)?;
            }
        }
    }
    for a in &config.accepted_gaps {
        if !gaps.iter().any(|g| g.id == a.gap) {
            writeln!(
                w,
                "⚠ `{}` is accepted as a gap but is not one now: remove its [[accept]] entry",
                a.gap
            )?;
        }
    }
    if open == 0 {
        writeln!(w, "✓ every metric is measured or its gap is accepted")?;
    } else {
        writeln!(
            w,
            "\n{open} gap(s) not accepted. Fix the cause and collect again, or accept a gap in \
             pmx.toml so it is a decision:\n\n    [[accept]]\n    gap = \"<id>\"\n    reason = \"<why this \
             project goes without it>\""
        )?;
    }
    Ok(open)
}
