use fleetix::supply::{self, CONTRACT, FactsDocument};
use serde_json::{Value, json};
use std::path::Path;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/supply")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn doc(producer: &str, body: Value) -> FactsDocument {
    let mut value = json!({
        "contract": CONTRACT,
        "vocabulary": "test.v1",
        "producer": {"name": producer, "version": "0"},
        "observedAt": "2026-10-26T00:00:00Z"
    });
    for (key, field) in body.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn repo(coordinate: &str, attrs: Value) -> Value {
    json!({"id": format!("repo:{coordinate}"), "kind": "repo", "attrs": attrs})
}

fn usage(coordinate: &str, kind: &str, state: &str) -> Value {
    json!({
        "id": format!("usage:{coordinate}#{kind}"),
        "kind": "usage",
        "attrs": {"usageKind": kind, "state": state}
    })
}

fn row(kind: &str, matches: Value, status: &str, lands: bool, extra: Value) -> Value {
    let mut value = json!({
        "usageKind": kind, "match": matches, "path": "p", "via": "v",
        "lands": lands, "status": status, "reason": "r"
    });
    for (key, field) in extra.as_object().unwrap() {
        value[key] = field.clone();
    }
    value
}

fn enrolled(coordinate: &str, orchestrator: &str, attrs: Value) -> Value {
    json!({
        "from": format!("repo:{coordinate}"),
        "to": format!("orchestrator:{orchestrator}"),
        "kind": "enrolledIn",
        "attrs": attrs
    })
}

/// `(node, blocker, subcode, owner)` for every gap.
fn gaps(docs: &[FactsDocument]) -> Vec<(String, String, Option<String>, Option<String>)> {
    supply::gaps(&supply::build(docs, None))
        .gaps
        .into_iter()
        .map(|gap| (gap.node, gap.blocker, gap.subcode, gap.owner))
        .collect()
}

fn blockers(docs: &[FactsDocument]) -> Vec<String> {
    gaps(docs).into_iter().map(|gap| gap.1).collect()
}

/// Refresh with `FLEETIX_BLESS=1 cargo test --test supply` after replacing a
/// producer fixture, then review the diff.
#[test]
fn fixtures_produce_expected_gaps() {
    let mut docs = supply::parse(&fixture("simit-usage-facts.v1.json")).unwrap();
    docs.extend(supply::parse(&fixture("canix-overlay.v1.json")).unwrap());
    let report = supply::gaps(&supply::build(&docs, None));
    let actual = serde_json::to_string_pretty(&report).unwrap() + "\n";
    let expected =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/supply/expected-gaps.json");
    if std::env::var_os("FLEETIX_BLESS").is_some() {
        std::fs::write(&expected, &actual).unwrap();
    }
    assert_eq!(actual, fixture("expected-gaps.json"));
}

#[test]
fn parse_accepts_one_document_or_an_array_and_rejects_other_contracts() {
    let one = json!({
        "contract": CONTRACT, "vocabulary": "v", "producer": {"name": "p", "version": "1"},
        "observedAt": "2026-10-26T00:00:00Z"
    });
    assert_eq!(supply::parse(&one.to_string()).unwrap().len(), 1);
    assert_eq!(
        supply::parse(&json!([one, one]).to_string()).unwrap().len(),
        2
    );

    let mut newer = one.clone();
    newer["contract"] = json!("fleetix.supply-facts.v2");
    let error = supply::parse(&json!([one, newer]).to_string()).unwrap_err();
    assert!(error.to_string().contains("unsupported facts contract"));

    let mut unknown = one.clone();
    unknown["extra"] = json!(true);
    assert!(supply::parse(&unknown.to_string()).is_err());
}

#[test]
fn r1_merges_by_id_and_reports_contradictions() {
    let first = doc(
        "simit",
        json!({
            "nodes": [
                {"id": "repo:h/o/r", "kind": "repo", "attrs": {"forge": "h", "landing": null}},
                usage("h/o/r", "ci.primary", "drift")
            ],
            "edges": [enrolled("h/o/r", "u/a", json!({"enabled": true}))]
        }),
    );
    let second = doc(
        "canix",
        json!({
            "nodes": [
                {"id": "repo:h/o/r", "kind": "repo", "attrs": {"forge": "other", "landing": "direct", "ciTarget": null}},
                {"id": "usage:h/o/r#ci.primary", "kind": "rev"}
            ],
            "edges": [enrolled("h/o/r", "u/a", json!({"enabled": false, "status": "kept"}))]
        }),
    );
    let graph = supply::build(&[first, second], None);
    let repo = &graph.nodes["repo:h/o/r"];
    assert_eq!(repo.attrs["forge"], "h", "the first value wins a conflict");
    assert_eq!(
        repo.attrs["landing"], "direct",
        "null never blocks a later value"
    );
    assert!(!repo.attrs.contains_key("ciTarget"));
    assert_eq!(repo.producers, ["simit", "canix"]);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].attrs["status"], "kept");

    let reasons: Vec<_> = graph
        .conflicts
        .iter()
        .map(|gap| gap.reason.as_str())
        .collect();
    assert_eq!(reasons.len(), 3, "{reasons:?}");
    assert!(reasons[0].contains("attr 'forge'"));
    assert!(reasons[1].contains("kind 'usage'"));
    assert!(reasons[2].contains("enrolledIn edge") && reasons[2].contains("attr 'enabled'"));
    assert!(
        graph
            .conflicts
            .iter()
            .all(|gap| gap.blocker == "contract-conflict")
    );
}

#[test]
fn r2_picks_the_most_specific_row_per_producer_and_ties_conflict() {
    let nodes = json!([
        repo("h/o/r", json!({"forge": "h", "ciTarget": "a/h"})),
        usage("h/o/r", "ci.primary", "drift")
    ]);
    let specific = doc(
        "simit",
        json!({"nodes": nodes, "capabilities": [
            row("ci.primary", json!({}), "planned", true, json!({"blocker": "generic"})),
            row("ci.primary", json!({"forge": "h"}), "planned", true, json!({"blocker": "forge"})),
            row("ci.primary", json!({"forge": "h", "ciTarget": "a/h"}), "planned", true, json!({"blocker": "exact"})),
            row("ci.primary", json!({"forge": "h", "ciTarget": "other"}), "planned", true, json!({"blocker": "mismatch"})),
            row("ci.publish", json!({}), "planned", true, json!({"blocker": "other-kind"}))
        ]}),
    );
    assert_eq!(blockers(&[specific]), ["exact"]);

    let tied = doc(
        "simit",
        json!({"nodes": nodes, "capabilities": [
            row("ci.primary", json!({"forge": "h"}), "implemented", true, json!({})),
            row("ci.primary", json!({"ciTarget": "a/h"}), "planned", true, json!({"blocker": "b"}))
        ]}),
    );
    assert_eq!(blockers(&[tied]), ["contract-conflict"]);

    // Identical rows from two documents of one producer are not a tie.
    let duplicate = doc(
        "simit",
        json!({"capabilities": [row("ci.primary", json!({}), "implemented", true, json!({}))]}),
    );
    let with_nodes = doc("simit", json!({"nodes": nodes}));
    assert!(blockers(&[duplicate.clone(), duplicate, with_nodes]).is_empty());
}

#[test]
fn r3_requires_an_implemented_landing_row() {
    let nodes = json!([
        repo("h/o/r", json!({})),
        usage("h/o/r", "readme.badges", "current")
    ]);
    let cases = [
        (
            row("readme.badges", json!({}), "implemented", true, json!({})),
            vec![],
        ),
        (
            row(
                "readme.badges",
                json!({}),
                "implemented",
                false,
                json!({"blocker": "no-orchestrator"}),
            ),
            vec!["no-orchestrator"],
        ),
        (
            row(
                "readme.badges",
                json!({}),
                "unsupported",
                true,
                json!({"blocker": "manual-pin"}),
            ),
            vec!["manual-pin"],
        ),
        (
            row("readme.badges", json!({}), "planned", true, json!({})),
            vec!["no-capability"],
        ),
    ];
    for (capability, expected) in cases {
        let facts = doc(
            "simit",
            json!({"nodes": nodes, "capabilities": [capability]}),
        );
        assert_eq!(blockers(&[facts]), expected);
    }
    assert_eq!(
        blockers(&[doc("simit", json!({"nodes": nodes}))]),
        ["no-capability"]
    );

    let fallback = doc(
        "simit",
        json!({"nodes": nodes, "capabilities": [
            row("readme.badges", json!({}), "planned", true, json!({"blocker": "b", "fallback": "run it"}))
        ]}),
    );
    let report = supply::gaps(&supply::build(&[fallback], None));
    assert_eq!(report.gaps[0].reason, "r (fallback: run it)");
    assert_eq!(report.gaps[0].owner.as_deref(), Some("simit"));
}

#[test]
fn r4_requires_an_enabled_enrollment_of_the_row_orchestrator() {
    let facts = doc(
        "simit",
        json!({
            "nodes": [repo("h/o/r", json!({"forge": "h"})), usage("h/o/r", "ci.primary", "drift")],
            "capabilities": [row("ci.primary", json!({"forge": "h"}), "implemented", true, json!({"orchestrator": "updater"}))]
        }),
    );
    let overlay = |edges: Value| doc("canix", json!({"edges": edges}));

    assert_eq!(
        gaps(std::slice::from_ref(&facts)),
        [(
            "usage:h/o/r#ci.primary".into(),
            "not-enrolled".into(),
            None,
            None
        )]
    );
    let other_kind = overlay(json!([enrolled(
        "h/o/r",
        "updater-ng/fleet",
        json!({"enabled": true})
    )]));
    assert_eq!(blockers(&[facts.clone(), other_kind]), ["not-enrolled"]);

    let disabled = overlay(json!([enrolled(
        "h/o/r",
        "updater/fleet",
        json!({"enabled": false, "blocker": "pr-required", "reason": "needs a PR"})
    )]));
    let report = supply::gaps(&supply::build(&[facts.clone(), disabled.clone()], None));
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].blocker, "operator-disabled");
    assert_eq!(report.gaps[0].subcode.as_deref(), Some("pr-required"));
    assert_eq!(report.gaps[0].reason, "needs a PR");
    assert_eq!(report.gaps[0].owner.as_deref(), Some("canix"));
    assert_eq!(report.summary.drift.uncovered, 1);

    let unflagged = overlay(json!([enrolled("h/o/r", "updater/fleet", json!({}))]));
    assert_eq!(blockers(&[facts.clone(), unflagged]), ["operator-disabled"]);

    let enabled = overlay(json!([
        enrolled("h/o/r", "updater/fleet", json!({"enabled": false})),
        enrolled("h/o/r", "updater/canary", json!({"enabled": true}))
    ]));
    let report = supply::gaps(&supply::build(&[facts.clone(), enabled], None));
    assert!(report.gaps.is_empty());
    assert_eq!(
        (report.summary.covered, report.summary.drift.covered),
        (1, 1)
    );

    // Another producer's enrolled orchestrator covers the usage on its own.
    let alternate = doc(
        "canix",
        json!({
            "edges": [enrolled("h/o/r", "converge/workspace", json!({"enabled": true}))],
            "capabilities": [row("ci.primary", json!({}), "implemented", true, json!({"orchestrator": "converge"}))]
        }),
    );
    assert!(gaps(&[facts.clone(), disabled.clone(), alternate]).is_empty());
    let alternate_unenrolled = doc(
        "canix",
        json!({"capabilities": [row("ci.primary", json!({}), "implemented", true, json!({"orchestrator": "converge"}))]}),
    );
    assert_eq!(
        blockers(&[facts, disabled, alternate_unenrolled]),
        ["not-enrolled", "operator-disabled"]
    );
}

#[test]
fn r5_reports_expectations_until_some_producer_supplies_the_attr() {
    let mut node = usage("h/o/r", "ci.primary", "current");
    node["expects"] = json!([{"attr": "generatorRevision", "blocker": "unknown-generator-revision", "reason": "why"}]);
    let facts = doc(
        "simit",
        json!({
            "nodes": [node],
            "capabilities": [row("ci.primary", json!({}), "implemented", true, json!({}))]
        }),
    );
    let overlay = |revision: Value| {
        doc(
            "canix",
            json!({"nodes": [{"id": "usage:h/o/r#ci.primary", "kind": "usage", "attrs": {"generatorRevision": revision}}]}),
        )
    };
    assert_eq!(
        gaps(std::slice::from_ref(&facts)),
        [(
            "usage:h/o/r#ci.primary".into(),
            "unknown-generator-revision".into(),
            None,
            Some("simit".into())
        )]
    );
    assert_eq!(
        blockers(&[facts.clone(), overlay(Value::Null)]),
        ["unknown-generator-revision"]
    );
    assert!(blockers(&[facts, overlay(json!("abc"))]).is_empty());
}

#[test]
fn r7_passes_producer_blockers_through_and_skips_absent_usages() {
    let mut absent = usage("h/o/r", "release.workflow", "absent");
    absent["blockers"] = json!([{"code": "ignored", "reason": "absent"}]);
    let facts = doc(
        "simit",
        json!({"nodes": [
            {"id": "repo:h/o/r", "kind": "repo", "blockers": [
                {"code": "provider-mismatch", "reason": "crow on github"},
                {"code": "config-conflict", "reason": "two sources", "owner": "operator"}
            ]},
            absent
        ]}),
    );
    let report = supply::gaps(&supply::build(&[facts], None));
    let found: Vec<_> = report
        .gaps
        .iter()
        .map(|gap| {
            (
                gap.repository.as_deref(),
                gap.blocker.as_str(),
                gap.owner.as_deref(),
                gap.source,
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            (
                Some("h/o/r"),
                "config-conflict",
                Some("operator"),
                "producer"
            ),
            (
                Some("h/o/r"),
                "provider-mismatch",
                Some("simit"),
                "producer"
            ),
        ]
    );
    assert_eq!((report.summary.repositories, report.summary.usages), (1, 0));
}

#[test]
fn walk_follows_edges_both_ways_within_depth() {
    let mut docs = supply::parse(&fixture("simit-usage-facts.v1.json")).unwrap();
    docs.extend(supply::parse(&fixture("canix-overlay.v1.json")).unwrap());
    let graph = supply::build(&docs, None);
    let ids = |report: &supply::PathReport| -> Vec<(String, usize)> {
        report
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.depth))
            .collect()
    };

    let rev = "rev:github.com/caniko/simit@de3325be17104fbdb0d70f1da38149d4016f1790";
    let who_pins = supply::walk(&graph, rev, true, 2).unwrap();
    assert_eq!(who_pins.contract, supply::PATH_CONTRACT);
    assert_eq!(
        ids(&who_pins),
        [
            (rev.to_string(), 0),
            (
                "usage:codefloe.com/caniko/pink-raven#flake-input.direct:simit".into(),
                1
            ),
            ("repo:codefloe.com/caniko/pink-raven".into(), 2),
        ]
    );
    assert_eq!(who_pins.edges.len(), 2);

    let repo = supply::walk(&graph, "github.com/caniko/dns-manager", false, 1).unwrap();
    assert_eq!(repo.root, "repo:github.com/caniko/dns-manager");
    assert_eq!(
        ids(&repo),
        [
            ("repo:github.com/caniko/dns-manager".into(), 0),
            ("usage:github.com/caniko/dns-manager#ci.primary".into(), 1),
            (
                "usage:github.com/caniko/dns-manager#readme.badges".into(),
                1
            ),
            ("orchestrator:canix-converge/workspace".into(), 1),
        ]
    );
    assert_eq!(
        supply::walk(&graph, "github.com/caniko/dns-manager", false, 0)
            .unwrap()
            .nodes
            .len(),
        1
    );
    assert!(supply::walk(&graph, "repo:missing", false, 3).is_err());
}
