//! `out/private/people.json` (spec §10.9, plan D64): the per-person facts a separate people report
//! reads. Built from what `collect` already has; nothing is fetched for it.

use chrono::NaiveDate;
use pm_classify::Classifier;
use pm_config::{CodeHostKind, Config, Identity};
use pm_git::Git;
use pm_metrics::model::{RepoInfo, VelocityDetail};
use pm_metrics::people::{
    CommitFact, PEOPLE_SCHEMA, PackageFact, PeopleFile, PeopleRepo, PeopleVelocity, PrFact, pr_number, pr_ref, web_url,
};
use pm_metrics::velocity::Package;

use crate::collect::RepoPlan;
use crate::pulls::RepoPrs;

pub struct PeopleInput<'a> {
    pub config: &'a Config,
    pub repos: &'a [RepoInfo],
    pub plans: &'a [RepoPlan],
    /// Every repo's commit facts (any order).
    pub commits: Vec<CommitFact>,
    pub as_of: NaiveDate,
    pub generated_at: String,
    pub velocity: Option<&'a VelocityDetail>,
    /// The packages when Velocity did not run ([`crate::velocity::packages_from_facts`]).
    pub unscored: Vec<Package>,
    /// `None` = peer review not measured.
    pub prs: Option<&'a [RepoPrs]>,
    pub identity: &'a Identity,
    pub classifier: &'a Classifier,
}

pub fn build(i: PeopleInput) -> PeopleFile {
    let c = i.config;
    let host = c.code_host.as_ref().map(|h| match h.kind {
        CodeHostKind::Github => "github".to_string(),
        CodeHostKind::Gitlab => "gitlab".to_string(),
    });
    let repos = i
        .repos
        .iter()
        .map(|info| {
            let cfg = c.repos.iter().find(|r| r.display_name() == info.name);
            let plan = i.plans.iter().find(|p| p.name == info.name);
            let remote = cfg
                .and_then(|r| r.url.clone())
                .or_else(|| plan.and_then(|p| Git::open(&p.dir).remote_url()));
            PeopleRepo {
                name: info.name.clone(),
                role: info.role.clone(),
                sha: info.sha.clone(),
                host: host.clone(),
                web_url: cfg
                    .and_then(|r| r.web_url.clone())
                    .map(|u| u.trim_end_matches('/').to_string())
                    .or_else(|| remote.as_deref().and_then(web_url)),
            }
        })
        .collect();

    let range_start = c.project.range_start;
    let mut commits: Vec<CommitFact> = i
        .commits
        .into_iter()
        .filter(|f| f.date >= range_start && f.date <= i.as_of)
        .collect();
    // Stable: each repo's own order (newest first) is kept within a day.
    commits.sort_by(|a, b| b.date.cmp(&a.date).then(a.repo.cmp(&b.repo)));

    let velocity = i.velocity.map(|v| PeopleVelocity {
        rubric: v.rubric.clone(),
        classifiers: v.classifiers.clone(),
        since: v.options.since,
    });
    let packages = match i.velocity {
        Some(v) => v.packages.iter().map(package).collect(),
        None => i.unscored.iter().map(package).collect(),
    };

    let prs = i.prs.map(|repos| {
        let mut rows = Vec::new();
        for r in repos {
            for pr in &r.prs {
                if let Some(input) = crate::pulls::input(r, pr, i.identity, i.classifier, true) {
                    rows.push((r, pr, input));
                }
            }
        }
        let inputs: Vec<_> = rows.iter().map(|(_, _, input)| input.clone()).collect();
        let mentors = pm_metrics::pr::mentors(&inputs);
        let mut out: Vec<PrFact> = rows
            .into_iter()
            .zip(mentors)
            .filter(|((_, _, input), _)| input.merged >= range_start && input.merged <= i.as_of)
            .filter_map(|((r, pr, input), mentors)| {
                Some(PrFact {
                    repo: r.repo.clone(),
                    number: pr_number(&pr.id)?,
                    author: input.author,
                    created_at: pr.created_at.to_rfc3339(),
                    merged_at: pr.merged_at.to_rfc3339(),
                    merged: input.merged,
                    cycle_ms: input.cycle_ms,
                    first_review_ms: input.first_review_ms,
                    reviewers: input.reviewers,
                    mentors,
                    stack: input.stack,
                    size: input.size,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.merged
                .cmp(&a.merged)
                .then(a.repo.cmp(&b.repo))
                .then(b.number.cmp(&a.number))
        });
        out
    });

    PeopleFile {
        schema: PEOPLE_SCHEMA.into(),
        project: c.project.name.clone(),
        range_start,
        as_of: i.as_of,
        generated_at: i.generated_at,
        breadth_roles: c.project.breadth_roles.clone(),
        ai_attribution: c.project.ai_attribution,
        repos,
        commits,
        packages,
        velocity,
        prs,
    }
}

fn package(p: &Package) -> PackageFact {
    PackageFact {
        id: p.id.clone(),
        kind: match (&p.author, &p.pr) {
            (Some(_), _) => "author-week",
            (None, Some(_)) => "pr",
            (None, None) => "ticket",
        }
        .to_string(),
        date: p.date,
        description: p.description.clone(),
        repos: p.repos.clone(),
        authors: p.authors.clone(),
        commits: p.commit_refs.clone(),
        added: p.added,
        points: p.points,
        size: p.size.clone(),
        axes: p.axes,
        tests: p.tests,
        debt: p.verification_debt(),
        pr: p.pr.as_deref().and_then(pr_ref),
    }
}
