use fleetix::build_train::*;
use std::collections::{BTreeMap, BTreeSet};

fn goal(name: &str) -> Goal {
    Goal {
        derivation: name.into(),
        output: "out".into(),
    }
}

fn train() -> Train {
    let mut train = Train::new("overflow".into(), 10);
    let graph: BTreeMap<_, _> = [
        ("heavy", vec![]),
        ("light", vec![]),
        ("dependent", vec![goal("heavy")]),
    ]
    .into_iter()
    .map(|(name, dependencies)| {
        (
            goal(name),
            Definition {
                output_path: format!("/store/{name}"),
                dependencies: dependencies.into_iter().collect(),
                operation: Operation::Build,
            },
        )
    })
    .collect();
    for root in ["heavy", "light", "dependent"] {
        train
            .submit(
                Request {
                    attempt: root.into(),
                    target: "checks".into(),
                    source: "source".into(),
                    roots: BTreeSet::from([goal(root)]),
                    activates: false,
                },
                graph.clone(),
                0,
            )
            .unwrap();
    }
    train
}

#[test]
fn filtered_placement_preserves_readiness_and_independent_failure() {
    let mut train = train();
    let local = train
        .dispatch_where(0, |g, _| g.derivation == "heavy")
        .unwrap()
        .unwrap();
    assert_eq!(local.goal, goal("heavy"));
    let remote = train
        .dispatch_where(0, |g, _| g.derivation != "heavy")
        .unwrap()
        .unwrap();
    assert_eq!(remote.goal, goal("light"));
    assert!(
        train
            .dispatch_where(0, |g, _| g.derivation != "heavy")
            .unwrap()
            .is_none()
    );
    train.finish(&remote, Err("independent failure".into()));
    train.finish(&local, Ok(()));
    let dependent = train
        .dispatch_where(1, |g, _| g.derivation != "heavy")
        .unwrap()
        .unwrap();
    assert_eq!(dependent.goal, goal("dependent"));
    train.finish(&dependent, Ok(()));
    assert_eq!(train.outcome("dependent").unwrap(), Outcome::Ready);
    assert!(matches!(
        train.outcome("light").unwrap(),
        Outcome::Failed(_)
    ));
}

#[test]
fn rejecting_every_goal_does_not_consume_or_mutate_work() {
    let mut train = train();
    let before = serde_json::to_value(&train).unwrap();
    assert!(train.dispatch_where(0, |_, _| false).unwrap().is_none());
    assert_eq!(serde_json::to_value(&train).unwrap(), before);
    assert!(train.dispatch(0).unwrap().is_some());
}

#[test]
fn filtered_workers_never_duplicate_one_derivation() {
    let mut train = Train::new("overflow".into(), 10);
    let out = goal("multi");
    let dev = Goal {
        output: "dev".into(),
        ..out.clone()
    };
    let graph = [out.clone(), dev.clone()]
        .into_iter()
        .map(|g| {
            (
                g.clone(),
                Definition {
                    output_path: format!("/store/multi-{}", g.output),
                    dependencies: BTreeSet::new(),
                    operation: Operation::Build,
                },
            )
        })
        .collect();
    train
        .submit(
            Request {
                attempt: "a".into(),
                target: "checks".into(),
                source: "source".into(),
                roots: BTreeSet::from([out, dev]),
                activates: false,
            },
            graph,
            0,
        )
        .unwrap();
    let running = train.dispatch_where(0, |_, _| true).unwrap().unwrap();
    assert!(train.dispatch_where(0, |_, _| true).unwrap().is_none());
    train.finish(&running, Ok(()));
    let valid = train.nodes.keys().cloned().collect();
    train.reconcile(&valid);
    assert_eq!(train.outcome("a").unwrap(), Outcome::Ready);
}
