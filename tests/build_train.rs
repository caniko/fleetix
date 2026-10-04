use fleetix::build_train::*;
use std::collections::{BTreeMap, BTreeSet};

fn goal(name: &str) -> Goal {
    Goal {
        derivation: name.into(),
        output: "out".into(),
    }
}

fn graph(entries: &[(&str, &[&str])]) -> Graph {
    entries
        .iter()
        .map(|(name, deps)| {
            (
                goal(name),
                Definition {
                    output_path: format!("/store/{name}"),
                    dependencies: deps.iter().map(|dep| goal(dep)).collect(),
                    operation: Operation::Build,
                },
            )
        })
        .collect()
}

fn request(id: &str, host: &str, root: &str) -> Request {
    Request {
        attempt: id.into(),
        target: host.into(),
        source: format!("source-{id}"),
        roots: BTreeSet::from([goal(root)]),
        activates: true,
    }
}

#[test]
fn late_join_reorders_only_pending_ready_work_and_completes_independently() {
    let mut train = Train::new("builder-policy".into(), 10);
    train
        .submit(
            request("a", "atlas", "atlas"),
            graph(&[
                ("atlas", &["busy", "exclusive", "shared"]),
                ("busy", &[]),
                ("exclusive", &[]),
                ("shared", &[]),
            ]),
            0,
        )
        .unwrap();
    let busy = train.dispatch(0).unwrap().unwrap();
    assert_eq!(busy.goal, goal("busy"));
    train
        .submit(
            request("m", "murph", "murph"),
            graph(&[("murph", &["shared"]), ("shared", &[])]),
            1,
        )
        .unwrap();
    assert!(matches!(
        train.nodes[&goal("busy")].state,
        NodeState::Running { .. }
    ));
    let shared = train.dispatch(1).unwrap().unwrap();
    assert_eq!(shared.goal, goal("shared"));
    train.finish(&shared, Ok(()));
    let murph = train.dispatch(2).unwrap().unwrap();
    assert_eq!(murph.goal, goal("murph"));
    train.finish(&murph, Ok(()));
    assert_eq!(train.outcome("m").unwrap(), Outcome::Ready);
    assert_eq!(train.outcome("a").unwrap(), Outcome::Pending);
    train.finish(&busy, Ok(()));
    assert_eq!(train.dispatch(3).unwrap().unwrap().goal, goal("exclusive"));
}

#[test]
fn cancellation_and_failure_do_not_poison_other_branches() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(
            request("a", "atlas", "atlas"),
            graph(&[("atlas", &["shared"]), ("shared", &[])]),
            0,
        )
        .unwrap();
    train
        .submit(
            request("m", "murph", "murph"),
            graph(&[("murph", &["shared"]), ("shared", &[])]),
            1,
        )
        .unwrap();
    train.cancel("m").unwrap();
    let shared = train.dispatch(1).unwrap().unwrap();
    train.finish(&shared, Err("compiler failure".into()));
    assert!(matches!(train.outcome("a").unwrap(), Outcome::Failed(_)));
    assert_eq!(train.outcome("m").unwrap(), Outcome::Cancelled);
    train
        .submit(
            request("s", "starlord", "independent"),
            graph(&[("independent", &[])]),
            2,
        )
        .unwrap();
    assert_eq!(
        train.dispatch(2).unwrap().unwrap().goal,
        goal("independent")
    );
}

#[test]
fn incompatible_graphs_and_cycles_are_transactionally_rejected() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(request("a", "atlas", "one"), graph(&[("one", &[])]), 0)
        .unwrap();
    let before = serde_json::to_value(&train).unwrap();
    let mut conflicting = graph(&[("one", &[])]);
    conflicting.get_mut(&goal("one")).unwrap().output_path = "/different".into();
    assert!(train
        .submit(request("m", "murph", "one"), conflicting, 1)
        .is_err());
    assert_eq!(before, serde_json::to_value(&train).unwrap());
    assert!(train
        .submit(
            request("m", "murph", "x"),
            graph(&[("x", &["y"]), ("y", &["x"])]),
            1
        )
        .is_err());
    assert_eq!(before, serde_json::to_value(&train).unwrap());
}

#[test]
fn recovery_rechecks_store_evidence_and_ignores_stale_worker_receipts() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(request("a", "atlas", "one"), graph(&[("one", &[])]), 0)
        .unwrap();
    let old = train.dispatch(0).unwrap().unwrap();
    let encoded = serde_json::to_vec(&train).unwrap();
    let mut restored: Train = serde_json::from_slice(&encoded).unwrap();
    restored.reconcile(&BTreeSet::new());
    let new = restored.dispatch(1).unwrap().unwrap();
    assert_ne!(old.serial, new.serial);
    restored.finish(&old, Ok(()));
    assert_eq!(restored.outcome("a").unwrap(), Outcome::Pending);
    restored.finish(&new, Ok(()));
    assert_eq!(restored.outcome("a").unwrap(), Outcome::Ready);
    restored.reconcile(&BTreeSet::new());
    assert_eq!(restored.outcome("a").unwrap(), Outcome::Pending);
}

#[test]
fn fence_blocks_dispatch_survives_restart_and_waits_for_drain() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(request("a", "atlas", "one"), graph(&[("one", &[])]), 0)
        .unwrap();
    let work = train.dispatch(0).unwrap().unwrap();
    let fence = train.fence("a").unwrap();
    assert!(!train.drained());
    assert!(train.dispatch(1).unwrap().is_none());
    assert!(train.release_fence(&fence).is_err());
    train.finish(&work, Ok(()));
    let mut restored: Train =
        serde_json::from_value(serde_json::to_value(&train).unwrap()).unwrap();
    assert!(restored.drained());
    assert!(restored.dispatch(1).unwrap().is_none());
    assert!(restored.release_fence("wrong-token").is_err());
    restored.release_fence(&fence).unwrap();
}

#[test]
fn aging_eventually_beats_shared_first_and_newer_activation_supersedes_old() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(request("old", "atlas", "old"), graph(&[("old", &[])]), 0)
        .unwrap();
    train
        .submit(request("new", "atlas", "new"), graph(&[("new", &[])]), 1)
        .unwrap();
    train
        .submit(request("m", "murph", "new"), graph(&[("new", &[])]), 1)
        .unwrap();
    assert_eq!(train.dispatch(11).unwrap().unwrap().goal, goal("old"));
    assert!(train.authorize_activation("old").is_err());
    let work = train.dispatch(11).unwrap().unwrap();
    train.finish(&work, Ok(()));
    train.authorize_activation("new").unwrap();
}

#[test]
fn exact_outputs_and_distinct_revisions_share_without_merging_attempts() {
    let mut train = Train::new("p".into(), 10);
    let mut outputs = graph(&[("same-drv", &[])]);
    let dev = Goal {
        derivation: "same-drv".into(),
        output: "dev".into(),
    };
    outputs.insert(
        dev.clone(),
        Definition {
            output_path: "/store/dev".into(),
            dependencies: BTreeSet::new(),
            operation: Operation::Restore,
        },
    );
    train
        .submit(request("a", "atlas", "same-drv"), outputs.clone(), 0)
        .unwrap();
    let mut m = request("m", "murph", "same-drv");
    m.roots = BTreeSet::from([dev]);
    train.submit(m, outputs, 0).unwrap();
    assert_eq!(train.requests.len(), 2);
    assert_eq!(train.nodes.len(), 2);
    assert_eq!(train.requests["a"].request.source, "source-a");
    assert_eq!(train.requests["m"].request.source, "source-m");
    assert_eq!(train.dispatch(0).unwrap().unwrap().goal.output, "dev");
}

#[test]
fn duplicate_submission_reattaches_but_cannot_change_frozen_identity() {
    let mut train = Train::new("p".into(), 10);
    let a = request("a", "atlas", "one");
    let g = graph(&[("one", &[])]);
    train.submit(a.clone(), g.clone(), 0).unwrap();
    train.submit(a, g.clone(), 10).unwrap();
    let mut changed = request("a", "atlas", "one");
    changed.source = "different".into();
    assert!(train.submit(changed, g, 11).is_err());
    assert_eq!(train.requests["a"].sequence, 1);
    assert_eq!(train.requests.len(), 1);
    let _: BTreeMap<String, RequestRecord> = train.requests;
}

#[test]
fn cached_parent_prunes_source_builds_and_their_failures() {
    let mut train = Train::new("p".into(), 10);
    let mut definitions = graph(&[("parent", &["compiler"]), ("compiler", &[])]);
    definitions.get_mut(&goal("parent")).unwrap().operation = Operation::Restore;
    train
        .submit(request("a", "atlas", "parent"), definitions, 0)
        .unwrap();
    train.nodes.get_mut(&goal("compiler")).unwrap().state =
        NodeState::Failed("unused compiler".into());
    let restore = train.dispatch(0).unwrap().unwrap();
    assert_eq!(restore.goal, goal("parent"));
    assert_eq!(restore.definition.operation, Operation::Restore);
    train.finish(&restore, Ok(()));
    assert_eq!(train.outcome("a").unwrap(), Outcome::Ready);
    assert!(train.dispatch(1).unwrap().is_none());
}

#[test]
fn cached_outputs_do_not_bypass_live_activation_admission() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(request("a", "atlas", "atlas"), graph(&[("atlas", &[])]), 0)
        .unwrap();
    train.reconcile(&BTreeSet::from([goal("atlas")]));
    train.hold("a").unwrap();
    assert_eq!(train.outcome("a").unwrap(), Outcome::Ready);
    assert!(train
        .authorize_activation("a")
        .unwrap_err()
        .contains("admission"));
    train.admit("a").unwrap();
    train.authorize_activation("a").unwrap();
}

#[test]
fn named_outputs_of_one_derivation_never_compile_concurrently() {
    let mut train = Train::new("p".into(), 10);
    let out = goal("multi-output");
    let dev = Goal {
        derivation: out.derivation.clone(),
        output: "dev".into(),
    };
    let definitions = BTreeMap::from([
        (
            out.clone(),
            Definition {
                output_path: "/store/out".into(),
                dependencies: BTreeSet::new(),
                operation: Operation::Build,
            },
        ),
        (
            dev.clone(),
            Definition {
                output_path: "/store/dev".into(),
                dependencies: BTreeSet::new(),
                operation: Operation::Build,
            },
        ),
    ]);
    let mut req = request("a", "atlas", "multi-output");
    req.roots.insert(dev);
    train.submit(req, definitions, 0).unwrap();
    let first = train.dispatch(0).unwrap().unwrap();
    assert!(
        train.dispatch(0).unwrap().is_none(),
        "one derivation occupied two workers"
    );
    train.finish(&first, Ok(()));
    assert!(train.dispatch(0).unwrap().is_some());
}

#[test]
fn late_join_on_a_sibling_output_prioritizes_the_shared_builder() {
    for joined_interest in ["admitted", "held", "cancelled"] {
        let mut train = Train::new("p".into(), 10);
        let dev = Goal {
            derivation: "shared".into(),
            output: "dev".into(),
        };
        let mut definitions = graph(&[
            ("atlas", &["busy", "exclusive", "shared"]),
            ("busy", &[]),
            ("exclusive", &[]),
            ("shared", &[]),
        ]);
        definitions.insert(
            dev.clone(),
            Definition {
                output_path: "/store/shared-dev".into(),
                dependencies: BTreeSet::new(),
                operation: Operation::Build,
            },
        );
        train
            .submit(request("a", "atlas", "atlas"), definitions.clone(), 0)
            .unwrap();
        let busy = train.dispatch(0).unwrap().unwrap();
        assert_eq!(busy.goal, goal("busy"));
        definitions.insert(
            goal("murph"),
            Definition {
                output_path: "/store/murph".into(),
                dependencies: BTreeSet::from([dev]),
                operation: Operation::Build,
            },
        );
        train
            .submit(request("m", "murph", "murph"), definitions, 1)
            .unwrap();
        match joined_interest {
            "held" => train.hold("m").unwrap(),
            "cancelled" => train.cancel("m").unwrap(),
            _ => {}
        }
        let chosen = train.dispatch(1).unwrap().unwrap();
        assert_eq!(
            chosen.goal.derivation,
            if joined_interest == "admitted" {
                "shared"
            } else {
                "exclusive"
            }
        );
        assert!(matches!(
            train.nodes[&busy.goal].state,
            NodeState::Running { .. }
        ));
    }
}

#[test]
fn one_request_selecting_two_outputs_has_one_builder_interest() {
    let mut train = Train::new("p".into(), 10);
    let mut definitions = graph(&[("shared", &[]), ("exclusive", &[])]);
    let dev = Goal {
        derivation: "shared".into(),
        output: "dev".into(),
    };
    definitions.insert(
        dev.clone(),
        Definition {
            output_path: "/store/shared-dev".into(),
            dependencies: BTreeSet::new(),
            operation: Operation::Build,
        },
    );
    let mut atlas = request("a", "atlas", "shared");
    atlas.roots.insert(dev);
    train.submit(atlas, definitions.clone(), 0).unwrap();
    train
        .submit(request("m", "murph", "exclusive"), definitions, 0)
        .unwrap();
    assert_eq!(train.dispatch(0).unwrap().unwrap().goal, goal("exclusive"));
}

#[test]
fn unneeded_restore_plan_cannot_replace_another_requests_source_work() {
    let mut train = Train::new("p".into(), 10);
    train
        .submit(
            request("a", "atlas", "atlas"),
            graph(&[("atlas", &["shared"]), ("shared", &[])]),
            0,
        )
        .unwrap();
    // A restored parent still carries canonical derivation dependencies, but
    // its dry-run does not request any work on those dependency outputs.
    let mut cached = graph(&[("murph", &["shared"]), ("shared", &[])]);
    for definition in cached.values_mut() {
        definition.operation = Operation::Restore;
    }
    train
        .submit(request("m", "murph", "murph"), cached, 1)
        .unwrap();
    let restore = train.dispatch(1).unwrap().unwrap();
    assert_eq!(restore.goal, goal("murph"));
    assert_eq!(restore.definition.operation, Operation::Restore);
    train.finish(&restore, Ok(()));
    let build = train.dispatch(1).unwrap().unwrap();
    assert_eq!(build.goal, goal("shared"));
    assert_eq!(build.definition.operation, Operation::Build);
    train.finish(&build, Ok(()));
    let atlas = train.dispatch(1).unwrap().unwrap();
    train.finish(&atlas, Ok(()));
    assert_eq!(train.outcome("a").unwrap(), Outcome::Ready);
    assert_eq!(train.outcome("m").unwrap(), Outcome::Ready);
}

#[test]
fn unselected_sibling_output_can_later_enter_the_source_frontier() {
    let mut train = Train::new("p".into(), 10);
    let dev = Goal {
        derivation: "shared".into(),
        output: "dev".into(),
    };
    let mut cached = graph(&[("shared", &[])]);
    cached.get_mut(&goal("shared")).unwrap().operation = Operation::Restore;
    cached.insert(
        dev.clone(),
        Definition {
            output_path: "/store/shared-dev".into(),
            dependencies: BTreeSet::new(),
            operation: Operation::Restore,
        },
    );
    train
        .submit(request("a", "atlas", "shared"), cached.clone(), 0)
        .unwrap();
    let out = train.dispatch(0).unwrap().unwrap();
    train.finish(&out, Ok(()));
    cached.get_mut(&dev).unwrap().operation = Operation::Build;
    let mut murph = request("m", "murph", "shared");
    murph.roots = BTreeSet::from([dev.clone()]);
    train.submit(murph, cached, 1).unwrap();
    let build = train.dispatch(1).unwrap().unwrap();
    assert_eq!(build.goal, dev);
    assert_eq!(build.definition.operation, Operation::Build);
    train.finish(&build, Ok(()));
    assert_eq!(train.outcome("a").unwrap(), Outcome::Ready);
    assert_eq!(train.outcome("m").unwrap(), Outcome::Ready);
}

#[test]
fn required_restore_goal_never_falls_through_to_source_compilation() {
    for held in [false, true] {
        let mut train = Train::new("p".into(), 10);
        let mut cached = graph(&[("shared", &[])]);
        cached.get_mut(&goal("shared")).unwrap().operation = Operation::Restore;
        train
            .submit(request("a", "atlas", "shared"), cached, 0)
            .unwrap();
        if held {
            train.hold("a").unwrap();
        }
        train
            .submit(
                request("m", "murph", "shared"),
                graph(&[("shared", &[])]),
                1,
            )
            .unwrap();
        let restore = train.dispatch(1).unwrap().unwrap();
        assert_eq!(restore.definition.operation, Operation::Restore);
        train.finish(&restore, Err("substitute disappeared".into()));
        assert!(matches!(train.outcome("a").unwrap(), Outcome::Failed(_)));
        assert!(matches!(train.outcome("m").unwrap(), Outcome::Failed(_)));
    }
}
