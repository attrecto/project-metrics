//! `out/private/people.json` (spec §10.9): commit facts, work packages without a classifier, links.

mod common;

use common::{BOB, BOT, JANE, Repo, d, lines};
use pm_config::Workspace;
use pmx::{CollectOptions, collect, collect::write_outputs};

#[test]
fn people_file_without_a_classifier() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let api = Repo::init(root.join("api"));
    api.write("src/a.ts", &lines("A", 0..10));
    api.commit("2025-02-03", JANE, "SHOP-1 add a");
    api.write("tests/a.test.ts", &lines("T", 0..4));
    api.commit(
        "2025-02-04",
        JANE,
        "SHOP-1 test a\n\nCo-Authored-By: Claude <noreply@anthropic.com>",
    );
    api.write("src/b.ts", &lines("B", 0..6));
    api.commit("2025-02-05", BOB, "Add b (#7)");
    api.write("src/c.ts", &lines("C", 0..3));
    api.commit("2025-02-11", BOB, "tweak c");
    api.write("package-lock.json", "{}\n");
    api.commit("2025-02-12", BOT, "bump deps");
    api.write("README.md", "# api\n");
    api.commit("2025-04-02", JANE, "SHOP-3 after as-of");
    std::fs::write(
        root.join("pmx.toml"),
        r#"
[project]
name = "Fixture"
range_start = "2025-01-01"
breadth_roles = ["backend"]

[[repo]]
path = "api"
branch = "main"
role = "backend"
web_url = "https://github.example.com/acme/api/"

[code_host]
type = "github"
"#,
    )
    .unwrap();
    let ws = Workspace::load(root).unwrap();
    let mut o = CollectOptions::new(d("2025-03-31"));
    o.fetch = false;
    o.snapshots = false;
    let c = collect(&ws, &o).unwrap();
    let p = &c.people;
    assert_eq!(p.schema, "pmx.people/1");
    assert_eq!(
        p.repos[0].web_url.as_deref(),
        Some("https://github.example.com/acme/api")
    );
    assert_eq!(p.repos[0].host.as_deref(), Some("github"));
    assert_eq!(p.repos[0].sha, api.head());

    // Bots and commits after as-of are out; newest first.
    let subjects: Vec<&str> = p.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["tweak c", "Add b (#7)", "SHOP-1 test a", "SHOP-1 add a"]);
    let test = &p.commits[2];
    assert!(test.ai && test.test && !test.prod);
    assert_eq!((test.person.as_str(), test.email.as_str()), ("Jane Doe", JANE.1));
    assert_eq!(p.commits[3].lines[&pm_classify::Role::Backend]["TypeScript"], 10);
    assert!(p.commits[3].prod && !p.commits[3].test);

    // No classifier: the packages are grouped anyway, unscored.
    assert!(p.velocity.is_none());
    let pkgs: Vec<(&str, &str)> = p.packages.iter().map(|p| (p.id.as_str(), p.kind.as_str())).collect();
    assert_eq!(
        pkgs,
        [
            ("Bob Builder~2025-W07", "author-week"),
            ("api#7", "pr"),
            ("SHOP-1", "ticket")
        ]
    );
    let pr = &p.packages[1];
    assert_eq!(pr.pr.as_ref().map(|r| (r.repo.as_str(), r.number)), Some(("api", 7)));
    let shop1 = &p.packages[2];
    assert_eq!(
        (shop1.commits.len(), shop1.added, shop1.tests, shop1.points),
        (2, 14, true, None)
    );
    assert_eq!(shop1.commits[0].sha, p.commits[3].sha);
    assert_eq!(shop1.description, "SHOP-1 add a");
    // The code host is set but no repo has a code_host_id: peer review not measured.
    assert!(p.prs.is_none());

    // Written next to leads.json, never into project.json.
    write_outputs(&ws.out_dir(), &c).unwrap();
    let written = std::fs::read_to_string(ws.out_dir().join("private/people.json")).unwrap();
    assert!(written.contains("Bob Builder"));
    let project = std::fs::read_to_string(ws.out_dir().join("project.json")).unwrap();
    assert!(!project.contains("Bob Builder") && !project.contains(JANE.1));
}
