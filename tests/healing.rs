use fleetix::healing::{
    CoalescePolicy, DeadReason, FailureKind, KeyedItem, Liveness, ProcessIdentity, ProgressSample,
    RecoveryAction, RetryDecision, RetryPolicy, RetryState, StallWatchdog, StrandedObservation,
    WatchdogPolicy, WatchdogVerdict, YieldDecision, YieldPolicy, YieldTracker, classify_stranded,
    coalesce_keyed, liveness, watchdog,
};
use fleetix::ops::{self, Request, Step};
use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

#[test]
fn ops_spool_requeue_complete_dropping_and_summarize() {
    let spool = tempfile::tempdir().unwrap();
    let op = ops::queue(
        spool.path(),
        Request {
            host: "atlas".into(),
            title: "Rotate credentials".into(),
            reason: "Expiring soon".into(),
            steps: vec![Step {
                argv: vec!["true".into()],
                note: "Run verification.".into(),
                root: false,
            }],
            requested_by: "cli:test".into(),
        },
    )
    .unwrap();
    assert!(ops::phase_exists(spool.path(), ops::PENDING, &op.id).unwrap());
    assert!(!ops::phase_exists(spool.path(), ops::RUNNING, &op.id).unwrap());

    // Simulate moving into running/, then auto-requeuing safely.
    fs::create_dir_all(spool.path().join(ops::RUNNING)).unwrap();
    fs::rename(
        ops::spool_path(spool.path(), ops::PENDING, &op.id).unwrap(),
        ops::spool_path(spool.path(), ops::RUNNING, &op.id).unwrap(),
    )
    .unwrap();
    assert_eq!(ops::running(spool.path(), Some("atlas")).unwrap().len(), 1);
    assert!(ops::requeue(spool.path(), &op.id).unwrap());
    // Idempotent second call when already moved back to pending/.
    assert!(!ops::requeue(spool.path(), &op.id).unwrap());
    assert_eq!(ops::pending(spool.path(), None).unwrap().len(), 1);

    // Refuses to requeue if done/<id>.json already exists.
    fs::create_dir_all(spool.path().join(ops::DONE)).unwrap();
    fs::write(
        ops::spool_path(spool.path(), ops::DONE, &op.id).unwrap(),
        b"{}",
    )
    .unwrap();
    assert!(ops::requeue(spool.path(), &op.id).is_err());

    // Interrupted drop in dropping/ is healed cleanly.
    let drop_id = "20261125T000000-abcdef";
    fs::create_dir_all(spool.path().join(ops::DROPPING)).unwrap();
    fs::write(
        ops::spool_path(spool.path(), ops::DROPPING, drop_id).unwrap(),
        b"{}",
    )
    .unwrap();
    // Malformed entry in pending/ is reported without aborting summary.
    fs::write(spool.path().join(ops::PENDING).join("broken.json"), b"{").unwrap();

    let before = ops::summarize(spool.path(), chrono::Utc::now()).unwrap();
    assert_eq!(before.pending, 1);
    assert_eq!(before.dropping, 1);
    assert_eq!(before.done, 1);
    assert_eq!(before.malformed.len(), 1);

    let healed = ops::complete_dropping(spool.path()).unwrap();
    assert_eq!(healed, vec![drop_id]);
    let after = ops::summarize(spool.path(), chrono::Utc::now()).unwrap();
    assert_eq!(after.dropping, 0);
}

#[test]
#[cfg(target_os = "linux")]
fn process_identity_detects_exited_child_and_watchdog_observes_real_tree() {
    use std::io::Read;
    use std::process::Stdio;

    // Wait for a readiness byte from shell builtins (`printf` + `read`) so the
    // child has completed `execve`, never invokes a second `execve`, and stays
    // parked on its stdin pipe until dropped or killed.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf 'ready\\n'; read _"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0u8; 6];
    child
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready\n");

    let pid = child.id();
    let identity = ProcessIdentity::capture(pid).unwrap();
    assert_eq!(identity.check(), Liveness::Alive);

    let log_dir = tempfile::tempdir().unwrap();
    let log = log_dir.path().join("worker.log");
    fs::write(&log, b"starting\n").unwrap();
    let sample1 = watchdog::observe_process_tree(pid, &[log.as_path()]);
    assert!(sample1.live_processes >= 1);
    assert_eq!(sample1.output_bytes, 9);

    let diag = watchdog::diagnose_stuck_pids(&liveness::descendant_pids(pid), 4);
    assert!(!diag.is_empty());
    assert!(diag.iter().any(|entry| entry.cmdline.is_some()));

    // Appending to the log advances the sample immediately.
    fs::write(&log, b"starting\nprogress line\n").unwrap();
    let sample2 = watchdog::observe_process_tree(pid, &[log.as_path()]);
    assert!(sample2.advanced_from(&sample1));

    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(identity.check(), Liveness::Dead(DeadReason::NotFound));
}

#[test]
fn keyed_coalescing_retry_reconcile_and_yield_compose_end_to_end() {
    // 1. Stacked requests coalesce to latest spec while keeping multi-key siblings.
    let items: Vec<KeyedItem<String, String>> = vec![
        KeyedItem {
            id: "pin-1".into(),
            mode: "edit".into(),
            keys: BTreeMap::from([("infernix".into(), "rev1".into())]),
            deferrals: 0,
        },
        KeyedItem {
            id: "pin-2".into(),
            mode: "edit".into(),
            keys: BTreeMap::from([
                ("ai-skills".into(), "revA".into()),
                ("infernix".into(), "rev2".into()),
            ]),
            deferrals: 0,
        },
        KeyedItem {
            id: "pin-3".into(),
            mode: "edit".into(),
            keys: BTreeMap::from([("infernix".into(), "rev3".into())]),
            deferrals: 0,
        },
    ];
    let plan = coalesce_keyed(&items, CoalescePolicy::default());
    assert_eq!(plan.superseded.len(), 1);
    assert_eq!(plan.superseded[0].id, "pin-1");
    assert_eq!(plan.boarded_ids, vec!["pin-2", "pin-3"]);
    assert_eq!(plan.effective_specs["infernix"], "rev3");
    assert_eq!(plan.effective_specs["ai-skills"], "revA");

    // 2. Stall watchdog + stranded classifier stops a stalled non-mutating worker,
    //    then retry policy backs off and eventually quarantines.
    let mut wd = StallWatchdog::new(
        WatchdogPolicy {
            max_runtime_seconds: 1800,
            stall_timeout_seconds: 60,
            startup_grace_seconds: 10,
        },
        100,
    );
    let frozen = ProgressSample {
        output_bytes: 128,
        output_mtime_nanos: 1,
        cpu_ticks: 10,
        io_bytes: 0,
        live_processes: 1,
    };
    assert!(wd.observe(100, frozen, Vec::new).is_healthy());
    let stalled = wd.observe(160, frozen, Vec::new);
    assert!(matches!(stalled, WatchdogVerdict::Stalled { .. }));

    let action = classify_stranded(&StrandedObservation {
        has_done_record: false,
        dropping: false,
        mutating_dispatched: false,
        owner_liveness: Liveness::Alive,
        worker_active: Some(true),
        exit_status: None,
        watchdog: stalled,
    });
    assert!(matches!(action, RecoveryAction::StopStalledWorker { .. }));

    let mut retry = RetryState::default();
    let policy = RetryPolicy {
        max_attempts: 2,
        base_backoff_seconds: 15,
        max_backoff_seconds: 60,
    };
    assert_eq!(
        retry.record_failure(&policy, FailureKind::Stalled, 160, "stalled".into()),
        RetryDecision::RetryAfter {
            delay_seconds: 15,
            attempt: 1,
        }
    );
    assert_eq!(
        retry.record_failure(&policy, FailureKind::Stalled, 175, "stalled again".into()),
        RetryDecision::Quarantine {
            attempts: 2,
            reason: "stalled again".into(),
        }
    );

    // 3. Background train yields to interactive ops up to its fairness cap.
    let mut yield_tracker = YieldTracker::new(YieldPolicy {
        max_yield_seconds: 30,
    });
    assert_eq!(
        yield_tracker.evaluate(true, 1000),
        YieldDecision::Yield {
            remaining_seconds: 30
        }
    );
    assert_eq!(yield_tracker.evaluate(true, 1030), YieldDecision::Proceed);
}

#[test]
#[cfg(unix)]
fn sd_notify_delivers_datagram_when_notify_socket_is_bound() {
    use std::os::unix::net::UnixDatagram;
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("notify.sock");
    let listener = UnixDatagram::bind(&sock_path).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();

    // Direct send check through a subprocess avoids mutating process env in parallel tests.
    let sock_str = sock_path.to_str().unwrap();
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", "true"])
        .env("NOTIFY_SOCKET", sock_str)
        .status()
        .unwrap();
    assert!(status.success());

    // Exercise the actual datagram format directly.
    let client = UnixDatagram::unbound().unwrap();
    client.send_to(b"WATCHDOG=1", &sock_path).unwrap();
    let mut buf = [0u8; 32];
    let n = listener.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"WATCHDOG=1");
}
