use super::*;
use serde_json::json;

const GIB: u64 = 1 << 30;
const OWNER: &str = "principal";
const OTHER: &str = "other-principal";

fn at(seconds: u32) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + i64::from(seconds), 0).unwrap()
}

fn hosts() -> BTreeMap<String, HostRole> {
    BTreeMap::from([
        ("atlas".into(), HostRole::Primary),
        ("nomad".into(), HostRole::Adjacent),
    ])
}

fn sample(host: &str, sequence: u64, seconds: u32) -> HostTelemetrySample {
    sample_for_role(host, hosts()[host], sequence, seconds)
}

fn sample_for_role(host: &str, role: HostRole, sequence: u64, seconds: u32) -> HostTelemetrySample {
    serde_json::from_value(json!({
        "schemaVersion": 1,
        "host": host,
        "role": role,
        "bootId": format!("{host}-boot"),
        "sequence": sequence,
        "observedAt": at(seconds).to_rfc3339(),
        "ttlSecs": 30,
        "attached": true,
        "acPower": true,
        "amcSource": "host",
        "amcAdmissible": true,
        "usableHeadroomBytes": 32 * GIB,
        "memoryFullAvg10": 0.1,
        "maxMemoryFullPsi": 1.0,
        "activeWorkloads": [],
        "workloadInventoryComplete": true,
        "projects": { "phemus": { "memoryMaxBytes": 24 * GIB, "swapMaxBytes": 0 } }
    }))
    .unwrap()
}

fn request(key: &str) -> PlacementRequest {
    PlacementRequest {
        idempotency_key: key.into(),
        holder: format!("holder-{key}"),
        prefer: vec!["nomad".into(), "atlas".into()],
        required_bytes: 5 * GIB,
        ..PlacementRequest::default()
    }
}

#[test]
fn placement_and_recommendation_use_caller_supplied_hosts() {
    let workers = BTreeMap::from([
        ("worker-a".into(), HostRole::Primary),
        ("worker-b".into(), HostRole::Adjacent),
    ]);
    let mut tracker = ComputeTracker::new(workers.clone(), "workers");
    for (host, role) in workers {
        tracker
            .record_sample(sample_for_role(&host, role, 0, 0), at(0))
            .unwrap();
    }
    let snapshot = tracker.snapshot(at(0));
    assert_eq!(snapshot.recommended_host, None);
    for (preferred, expected) in [
        (["missing", "worker-b", "worker-a"], "worker-b"),
        (["missing", "worker-a", "worker-b"], "worker-a"),
    ] {
        assert_eq!(
            tracker
                .snapshot_with_preference(at(0), &preferred)
                .recommended_host
                .as_deref(),
            Some(expected)
        );
    }

    let mut req = PlacementRequest {
        idempotency_key: "job".into(),
        holder: "worker".into(),
        required_bytes: 5 * GIB,
        host_required_bytes: BTreeMap::from([("worker-b".into(), 8 * GIB)]),
        ..PlacementRequest::default()
    };
    assert!(req.prefer.is_empty());
    assert_eq!(snapshot.eligible_host_for(&req), None);
    assert_eq!(
        tracker.try_claim(req.clone(), "job", OWNER, at(0)),
        Ok(ClaimOutcome::Unavailable)
    );
    assert_eq!(req.required_bytes_for("worker-a"), 5 * GIB);
    assert_eq!(req.required_bytes_for("worker-b"), 8 * GIB);
    req.prefer = vec!["worker-b".into(), "worker-a".into()];
    let claim = grant(&mut tracker, req, "job", 0);
    assert_eq!(claim.host, "worker-b");
    assert_eq!(claim.reserved_bytes, 8 * GIB);
    assert_eq!(
        tracker
            .snapshot_with_preference(at(0), &["worker-b", "worker-a"])
            .recommended_host
            .as_deref(),
        Some("worker-a")
    );
}

fn only(host: &str, key: &str) -> PlacementRequest {
    PlacementRequest {
        allow_hosts: Some(vec![host.into()]),
        ..request(key)
    }
}

fn grant(
    tracker: &mut ComputeTracker,
    req: PlacementRequest,
    id: &str,
    seconds: u32,
) -> ComputeClaim {
    match tracker.try_claim(req, id, OWNER, at(seconds)).unwrap() {
        ClaimOutcome::Granted(claim) => claim,
        ClaimOutcome::Unavailable => panic!("{id} was not granted"),
    }
}

fn running(claim: &ComputeClaim, name: &str) -> HeartbeatRequest {
    HeartbeatRequest {
        holder: claim.holder.clone(),
        ttl_secs: None,
        stage: Some(ClaimStage::Running),
        unit: Some(name.into()),
    }
}

#[test]
fn dropped_response_and_restart_do_not_duplicate_a_live_remote_unit() {
    let mut tracker = ComputeTracker::new(hosts(), "first");
    tracker.record_sample(sample("atlas", 0, 0), at(0)).unwrap();
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let req = request("remote");
    let granted = grant(&mut tracker, req.clone(), "claim", 0);
    assert_eq!(granted.host, "nomad");
    assert_eq!(grant(&mut tracker, req.clone(), "ignored", 1), granted);
    tracker
        .heartbeat(
            "claim",
            running(&granted, "canix-adjacent-live"),
            OWNER,
            at(2),
        )
        .unwrap();
    assert!(tracker.advance(at(92)));
    let mut tracker =
        ComputeTracker::from_persisted(hosts(), "second", tracker.persisted_state(), at(93))
            .unwrap();
    let snapshot = tracker.snapshot(at(93));
    assert_eq!(snapshot.hosts["nomad"].state, HostAvailability::Unknown);
    assert_eq!(snapshot.hosts["atlas"].state, HostAvailability::Unknown);
    assert_eq!(
        snapshot.hosts["nomad"].active_claims[0].phase,
        ClaimPhase::Reconciling
    );

    tracker
        .record_sample(sample("atlas", 1, 94), at(94))
        .unwrap();
    let mut live = sample("nomad", 1, 94);
    live.active_workloads.push(ObservedWorkload {
        kind: "adjacentRun".into(),
        name: "canix-adjacent-live".into(),
        project: None,
        memory_current_bytes: None,
    });
    tracker.record_sample(live, at(94)).unwrap();
    assert!(matches!(
        tracker.try_claim(req, "must-not-fallback", OWNER, at(94)),
        Err(ComputeError::ClaimReconciling)
    ));
    let other = only("nomad", "other");
    assert_eq!(
        tracker
            .try_claim(other.clone(), "blocked", OWNER, at(94))
            .unwrap(),
        ClaimOutcome::Unavailable
    );
    assert_eq!(
        tracker.snapshot(at(94)).hosts["nomad"].state,
        HostAvailability::Busy
    );

    tracker
        .record_sample(sample("nomad", 2, 95), at(95))
        .unwrap();
    let view = &tracker.snapshot(at(95)).hosts["nomad"];
    assert!(view.active_claims.is_empty());
    assert_eq!(view.state, HostAvailability::Free);
    assert!(grant(&mut tracker, other, "next", 95).generation > granted.generation);
}

#[test]
fn atlas_keeps_start_debit_until_a_newer_sample_and_honors_host_override() {
    let mut tracker = ComputeTracker::new(hosts(), "atlas");
    let mut atlas = sample("atlas", 0, 0);
    atlas.usable_headroom_bytes = Some(8 * GIB);
    atlas.local_slots = Some(1);
    atlas.occupied_slots = Some(0);
    tracker.record_sample(atlas.clone(), at(0)).unwrap();
    let mut first = only("atlas", "first");
    first.required_bytes = 24 * GIB;
    first.host_required_bytes.insert("atlas".into(), 5 * GIB);
    let claim = grant(&mut tracker, first, "first", 0);
    assert_eq!(claim.reserved_bytes, 5 * GIB);
    assert!(!claim.exclusive);
    let view = &tracker.snapshot(at(0)).hosts["atlas"];
    assert_eq!(view.pending_start_reserved_bytes, 5 * GIB);
    assert_eq!(view.remaining_headroom_bytes, Some(3 * GIB));
    let second = only("atlas", "second");
    assert_eq!(tracker.is_eligible_for_request(&second, OWNER, at(0)), None);
    assert_eq!(
        tracker
            .try_claim(second.clone(), "second", OWNER, at(0))
            .unwrap(),
        ClaimOutcome::Unavailable
    );

    atlas.sequence = 1;
    atlas.observed_at = at(1);
    atlas.usable_headroom_bytes = Some(6 * GIB);
    atlas.committed_bytes = Some(5 * GIB);
    tracker.record_sample(atlas.clone(), at(1)).unwrap();
    assert_eq!(tracker.is_eligible_for_request(&second, OWNER, at(1)), None);
    assert_eq!(
        tracker
            .heartbeat("first", running(&claim, "amc-build"), OWNER, at(1))
            .unwrap()
            .started_at,
        Some(at(1))
    );
    let view = &tracker.snapshot(at(1)).hosts["atlas"];
    assert_eq!(view.pending_start_reserved_bytes, 5 * GIB);
    assert_eq!(view.remaining_headroom_bytes, Some(GIB));
    assert_eq!(view.reasons, ["no local slots available"]);
    assert_eq!(tracker.is_eligible_for_request(&second, OWNER, at(1)), None);
    // Even after a newer sample arrives, a Running claim without an observed
    // AMC workload (or a non-AMC fallback unit) keeps its debit until AMC
    // telemetry confirms accounting.
    atlas.sequence = 2;
    atlas.observed_at = at(2);
    tracker.record_sample(atlas.clone(), at(2)).unwrap();
    let view = &tracker.snapshot(at(2)).hosts["atlas"];
    assert_eq!(view.pending_start_reserved_bytes, 5 * GIB);
    assert_eq!(view.remaining_headroom_bytes, Some(GIB));

    // A non-AMC fallback workload does not retire the debit either.
    atlas.sequence = 3;
    atlas.observed_at = at(3);
    atlas.active_workloads.push(ObservedWorkload {
        kind: "compute".into(),
        name: "amc-build".into(),
        project: None,
        memory_current_bytes: None,
    });
    tracker.record_sample(atlas.clone(), at(3)).unwrap();
    assert_eq!(
        tracker.snapshot(at(3)).hosts["atlas"].pending_start_reserved_bytes,
        5 * GIB
    );

    // Once a fresh AMC sample observes the running AMC unit, AMC's own headroom
    // accounts for it and the Hub debit retires cleanly.
    atlas.sequence = 4;
    atlas.observed_at = at(4);
    atlas.active_workloads = vec![ObservedWorkload {
        kind: "amc".into(),
        name: "amc-build".into(),
        project: None,
        memory_current_bytes: None,
    }];
    tracker.record_sample(atlas, at(4)).unwrap();
    assert_eq!(
        tracker
            .heartbeat("first", running(&claim, "amc-build"), OWNER, at(5))
            .unwrap()
            .started_at,
        Some(at(1))
    );
    let view = &tracker.snapshot(at(5)).hosts["atlas"];
    assert_eq!(view.pending_start_reserved_bytes, 0);
    assert_eq!(view.remaining_headroom_bytes, Some(6 * GIB));
    assert_eq!(view.state, HostAvailability::Free);
    assert_eq!(grant(&mut tracker, second, "second", 5).host, "atlas");
}

#[test]
fn nominal_request_cannot_bypass_reported_nomad_project_ceiling() {
    let mut tracker = ComputeTracker::new(hosts(), "nomad");
    let mut req = only("nomad", "project");
    req.project = Some("phemus".into());
    req.kind = ClaimKind::AdjacentRun;
    let mut nominal = sample("nomad", 0, 0);
    nominal.usable_headroom_bytes = Some(23 * GIB);
    tracker.record_sample(nominal, at(0)).unwrap();
    assert_eq!(tracker.is_eligible_for_request(&req, OWNER, at(0)), None);
    let mut unconfigured = sample("nomad", 1, 1);
    unconfigured.projects.clear();
    tracker.record_sample(unconfigured, at(1)).unwrap();
    assert_eq!(tracker.is_eligible_for_request(&req, OWNER, at(1)), None);
    let mut ready = sample("nomad", 2, 2);
    ready.memory_full_avg10 = ready.max_memory_full_psi;
    tracker.record_sample(ready, at(2)).unwrap();
    let mut missing = req.clone();
    missing.project = None;
    let mut oversized = req.clone();
    oversized
        .host_required_bytes
        .insert("nomad".into(), 24 * GIB + 1);
    let snap = tracker.snapshot(at(2));
    for rejected in [&missing, &oversized] {
        assert_eq!(snap.eligible_host_for(rejected), None);
        assert_eq!(
            tracker.is_eligible_for_request(rejected, OWNER, at(2)),
            None
        );
    }
    assert_eq!(snap.eligible_host_for(&req).as_deref(), Some("nomad"));
    let claim = grant(&mut tracker, req, "project", 2);
    assert_eq!(claim.reserved_bytes, 24 * GIB);
    assert!(claim.exclusive);
}

#[test]
fn nomad_detach_or_power_uncertainty_falls_back_except_during_reconciliation() {
    for (attached, ac_power) in [
        (Some(false), Some(true)),
        (Some(true), Some(false)),
        (Some(true), None),
        (None, Some(true)),
    ] {
        let mut tracker = ComputeTracker::new(hosts(), "fallback");
        tracker.record_sample(sample("atlas", 0, 0), at(0)).unwrap();
        tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
        let req = request("started");
        let claim = grant(&mut tracker, req.clone(), "started", 0);
        tracker
            .heartbeat("started", running(&claim, "remote"), OWNER, at(1))
            .unwrap();
        let mut detached = sample("nomad", 1, 2);
        detached.attached = attached;
        detached.ac_power = ac_power;
        tracker.record_sample(detached, at(2)).unwrap();
        assert_eq!(
            tracker.snapshot(at(2)).hosts["nomad"].state,
            HostAvailability::Detached
        );
        let other = request("waiter");
        tracker
            .enqueue_waiter(other.clone(), OWNER, at(20), at(2))
            .unwrap();
        assert_eq!(tracker.head_ready_waiter(at(2)).unwrap().request, other);
        assert_eq!(grant(&mut tracker, other, "fallback", 2).host, "atlas");
        assert_eq!(
            tracker
                .release(
                    "started",
                    ReleaseRequest {
                        holder: claim.holder,
                        unit_confirmed_stopped: false,
                        never_started: true,
                    },
                    OWNER,
                    at(3)
                )
                .unwrap(),
            ReleaseOutcome::Reconciling
        );
        assert_eq!(tracker.is_eligible_for_request(&req, OWNER, at(3)), None);
        assert_eq!(
            tracker.try_claim(req, "duplicate", OWNER, at(3)),
            Err(ComputeError::ClaimReconciling)
        );
    }
}

#[test]
fn nomad_needs_known_headroom_pressure_and_an_empty_workload_inventory() {
    let mut tracker = ComputeTracker::new(hosts(), "unknown");
    let mut nomad = sample("nomad", 0, 0);
    nomad.usable_headroom_bytes = None;
    tracker.record_sample(nomad, at(0)).unwrap();
    let snap = tracker.snapshot(at(0));
    assert_eq!(snap.hosts["nomad"].state, HostAvailability::Unknown);
    assert_eq!(snap.hosts["nomad"].remaining_headroom_bytes, None);
    let mut nomad = sample("nomad", 1, 1);
    nomad.max_memory_full_psi = None;
    tracker.record_sample(nomad, at(1)).unwrap();
    assert_eq!(
        tracker.snapshot(at(1)).hosts["nomad"].state,
        HostAvailability::Unknown
    );
    let mut nomad = sample("nomad", 2, 2);
    nomad.memory_full_avg10 = Some(2.0);
    tracker.record_sample(nomad, at(2)).unwrap();
    assert_eq!(
        tracker.snapshot(at(2)).hosts["nomad"].state,
        HostAvailability::Saturated
    );
    let mut nomad = sample("nomad", 3, 3);
    nomad.active_workloads.push(ObservedWorkload {
        kind: "nixOffload".into(),
        name: "nix-offload.service".into(),
        project: None,
        memory_current_bytes: Some(GIB),
    });
    tracker.record_sample(nomad, at(3)).unwrap();
    assert_eq!(
        tracker.snapshot(at(3)).hosts["nomad"].state,
        HostAvailability::Busy
    );
    assert_eq!(
        tracker.is_eligible_for_request(&request("blocked"), OWNER, at(3)),
        None
    );
    let mut nomad = sample("nomad", 4, 4);
    nomad.workload_inventory_complete = false;
    tracker.record_sample(nomad, at(4)).unwrap();
    let snap = tracker.snapshot(at(4));
    assert_eq!(snap.hosts["nomad"].state, HostAvailability::Unknown);
    assert_eq!(
        snap.hosts["nomad"].reasons,
        ["workload inventory incomplete"]
    );
    assert_eq!(snap.eligible_host_for(&request("blocked")), None);
}

#[test]
fn fifo_is_per_eligible_host_and_renewal_keeps_position_and_bound() {
    let mut tracker = ComputeTracker::with_max_waiters(hosts(), "queue", 2);
    let first = only("nomad", "first");
    let second = only("nomad", "second");
    let third = only("nomad", "third");
    tracker
        .enqueue_waiter(first.clone(), OWNER, at(5), at(0))
        .unwrap();
    tracker
        .enqueue_waiter(second.clone(), OWNER, at(20), at(1))
        .unwrap();
    assert_eq!(
        tracker.enqueue_waiter(third.clone(), OWNER, at(20), at(1)),
        Err(ComputeError::QueueFull)
    );
    tracker
        .enqueue_waiter(first.clone(), OWNER, at(25), at(2))
        .unwrap();

    tracker.record_sample(sample("atlas", 0, 5), at(5)).unwrap();
    assert!(tracker.head_ready_waiter(at(5)).is_none());
    assert_eq!(
        grant(&mut tracker, only("atlas", "separate"), "separate", 5).host,
        "atlas"
    );
    tracker.record_sample(sample("nomad", 0, 6), at(6)).unwrap();
    let head = tracker.head_ready_waiter(at(6)).unwrap();
    assert_eq!(head.request, first);
    assert_eq!(head.enqueued_at, at(0));
    assert_eq!(head.expires_at, at(25));
    for req in [second.clone(), third.clone()] {
        assert_eq!(
            tracker.try_claim(req, "skipped", OWNER, at(6)).unwrap(),
            ClaimOutcome::Unavailable
        );
    }
    let granted = grant(&mut tracker, first, "first-claim", 6);
    assert!(tracker.head_ready_waiter(at(6)).is_none());
    tracker
        .enqueue_waiter(third.clone(), OWNER, at(20), at(6))
        .unwrap();
    tracker
        .release(
            "first-claim",
            ReleaseRequest {
                holder: granted.holder,
                unit_confirmed_stopped: false,
                never_started: true,
            },
            OWNER,
            at(6),
        )
        .unwrap();
    assert_eq!(tracker.head_ready_waiter(at(6)).unwrap().request, second);
    assert!(!tracker.remove_waiter("second", OTHER, at(6)));
    assert!(tracker.remove_waiter("second", OWNER, at(6)));
    assert_eq!(tracker.head_ready_waiter(at(6)).unwrap().request, third);
    assert!(tracker.advance(at(20)));
    assert_eq!(tracker.snapshot(at(20)).waiter_count, 0);
}

#[test]
fn an_older_nomad_ticket_cannot_change_targets_and_skip_atlas_fifo() {
    let mut tracker = ComputeTracker::new(hosts(), "fifo");
    let nomad = only("nomad", "first");
    let atlas = only("atlas", "second");
    tracker
        .enqueue_waiter(nomad.clone(), OWNER, at(20), at(0))
        .unwrap();
    tracker
        .enqueue_waiter(atlas.clone(), OTHER, at(20), at(1))
        .unwrap();
    tracker.record_sample(sample("atlas", 0, 2), at(2)).unwrap();
    assert_eq!(
        tracker.enqueue_waiter(nomad.clone(), OTHER, at(30), at(2)),
        Err(ComputeError::RequestMismatch)
    );
    assert_eq!(
        tracker.try_claim(nomad.clone(), "wrong-owner", OTHER, at(2)),
        Err(ComputeError::RequestMismatch)
    );
    assert_eq!(tracker.is_eligible_for_request(&atlas, OWNER, at(2)), None);
    let mut changed = nomad;
    changed.allow_hosts.clone_from(&atlas.allow_hosts);
    assert_eq!(
        tracker.enqueue_waiter(changed.clone(), OWNER, at(30), at(2)),
        Err(ComputeError::RequestMismatch)
    );
    assert_eq!(
        tracker.try_claim(changed, "skip", OWNER, at(2)),
        Err(ComputeError::RequestMismatch)
    );
    let head = tracker.head_ready_waiter(at(2)).unwrap();
    assert_eq!(head.request, atlas);
    assert_eq!(head.owner_principal, OTHER);
}

#[test]
fn uncertain_pending_release_blocks_atlas_until_authoritative_inventory() {
    let mut tracker = ComputeTracker::new(hosts(), "release");
    tracker.record_sample(sample("atlas", 0, 0), at(0)).unwrap();
    let pending = grant(&mut tracker, only("atlas", "launch"), "launch", 0);
    let release: ReleaseRequest = serde_json::from_value(json!({
        "holder": pending.holder, "unitConfirmedStopped": false
    }))
    .unwrap();
    assert!(!release.never_started);
    assert_eq!(
        tracker.release(&pending.id, release, OWNER, at(1)),
        Ok(ReleaseOutcome::Reconciling)
    );
    let req = only("atlas", "next");
    let mut unknown = sample("atlas", 1, 2);
    unknown.workload_inventory_complete = false;
    tracker.record_sample(unknown, at(2)).unwrap();
    let snap = tracker.snapshot(at(2));
    assert_eq!(snap.hosts["atlas"].state, HostAvailability::Busy);
    assert_eq!(snap.hosts["atlas"].reasons, ["host has reconciling claim"]);
    assert_eq!(snap.eligible_host_for(&req), None);
    assert_eq!(
        tracker.try_claim(req.clone(), "blocked", OWNER, at(2)),
        Ok(ClaimOutcome::Unavailable)
    );
    tracker.record_sample(sample("atlas", 2, 3), at(3)).unwrap();
    let next = grant(&mut tracker, req, "next", 3);
    assert_eq!(
        tracker.release(
            &next.id,
            ReleaseRequest {
                holder: next.holder,
                unit_confirmed_stopped: false,
                never_started: true,
            },
            OWNER,
            at(4),
        ),
        Ok(ReleaseOutcome::Released)
    );
}

#[test]
fn atlas_budget_and_contract_limit_match_snapshot_placement() {
    let mut tracker = ComputeTracker::new(hosts(), "limits");
    let mut atlas = sample("atlas", 0, 0);
    atlas.usable_budget_bytes = Some(16 * GIB);
    atlas.contract_memory_max_bytes = Some(12 * GIB);
    tracker.record_sample(atlas, at(0)).unwrap();
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let mut req = request("too-large-for-atlas");
    req.prefer.reverse();
    req.required_bytes = 12 * GIB + 1;
    let snap = tracker.snapshot(at(0));
    assert_eq!(
        snap.hosts["atlas"].contract_memory_max_bytes,
        Some(12 * GIB)
    );
    assert_eq!(snap.hosts["atlas"].remaining_headroom_bytes, Some(16 * GIB));
    assert_eq!(snap.eligible_host_for(&req).as_deref(), Some("nomad"));
    assert_eq!(
        snap.eligible_host_for(&req),
        tracker.is_eligible_for_request(&req, OWNER, at(0))
    );
    req.allow_hosts = Some(vec!["atlas".into()]);
    assert_eq!(snap.eligible_host_for(&req), None);
    assert_eq!(
        tracker.try_claim(req, "over-contract", OWNER, at(0)),
        Ok(ClaimOutcome::Unavailable)
    );

    let mut first = only("atlas", "first");
    first.host_required_bytes.insert("atlas".into(), 8 * GIB);
    grant(&mut tracker, first, "first", 0);
    let mut second = only("atlas", "second");
    second.required_bytes = 8 * GIB + 1;
    assert_eq!(tracker.snapshot(at(0)).eligible_host_for(&second), None);
    assert_eq!(tracker.is_eligible_for_request(&second, OWNER, at(0)), None);
    second.required_bytes = 8 * GIB;
    grant(&mut tracker, second, "second", 0);
    let snap = tracker.snapshot(at(0));
    assert_eq!(snap.hosts["atlas"].remaining_headroom_bytes, Some(0));
    assert_eq!(snap.hosts["atlas"].state, HostAvailability::Saturated);
    assert_eq!(snap.eligible_host_for(&only("atlas", "blocked")), None);
}

#[test]
fn staging_on_a_restored_clone_keeps_custom_wait_limit_and_original_state() {
    let tracker = ComputeTracker::new(hosts(), "before");
    let mut restored = ComputeTracker::from_persisted_with_max_waiters(
        hosts(),
        "after",
        tracker.persisted_state(),
        1,
        at(0),
    )
    .unwrap();
    restored
        .record_sample(sample("atlas", 0, 0), at(0))
        .unwrap();
    let mut staged = restored.clone();
    staged.record_sample(sample("atlas", 1, 1), at(1)).unwrap();
    staged
        .enqueue_waiter(request("first"), OWNER, at(20), at(1))
        .unwrap();
    assert_eq!(
        staged.enqueue_waiter(request("second"), OWNER, at(20), at(1)),
        Err(ComputeError::QueueFull)
    );
    assert_eq!(restored.revision(), 1);
    assert_eq!(restored.snapshot(at(1)).waiter_count, 0);
    assert_eq!(
        restored.snapshot(at(1)).hosts["atlas"].observed_at,
        Some(at(0))
    );
    assert_eq!(
        staged.persisted_state().host_watermarks["atlas"].last_sequence,
        1
    );
}

#[test]
fn reboot_fences_active_claim_and_old_boot_replay_is_rejected() {
    let mut tracker = ComputeTracker::new(hosts(), "boots");
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    assert_eq!(
        tracker.record_sample(sample("nomad", 0, 1), at(1)),
        Err(ComputeError::StaleSequence)
    );
    tracker.record_sample(sample("nomad", 3, 1), at(1)).unwrap();
    assert_eq!(
        tracker.record_sample(sample("nomad", 2, 2), at(2)),
        Err(ComputeError::StaleSequence)
    );
    let old = grant(&mut tracker, request("old"), "old", 2);
    let mut reboot = sample("nomad", 0, 2);
    reboot.boot_id = "next-boot".into();
    reboot.workload_inventory_complete = false;
    tracker.record_sample(reboot, at(2)).unwrap();
    assert_eq!(
        tracker.record_sample(sample("nomad", 99, 4), at(4)),
        Err(ComputeError::StaleBoot)
    );
    assert_eq!(
        tracker.heartbeat("old", running(&old, "never"), OWNER, at(4)),
        Err(ComputeError::UnknownClaim)
    );
    let mut verified = sample("nomad", 1, 4);
    verified.boot_id = "next-boot".into();
    tracker.record_sample(verified, at(4)).unwrap();
    let new = grant(&mut tracker, request("new"), "new", 4);
    assert!(new.generation > old.generation);
    assert_eq!(new.host_boot_id, "next-boot");
}

#[test]
fn watermarks_reject_old_boots_and_sequences_after_restart_without_restoring_samples() {
    let mut tracker = ComputeTracker::new(hosts(), "old");
    tracker.record_sample(sample("nomad", 1, 9), at(9)).unwrap();
    let mut current = sample("nomad", 2, 10);
    current.boot_id = "current-boot".into();
    tracker.record_sample(current.clone(), at(10)).unwrap();
    let claim = grant(&mut tracker, request("current"), "current", 10);
    let persisted = tracker.persisted_state();
    let watermark = &persisted.host_watermarks["nomad"];
    assert_eq!(watermark.boot_id, "current-boot");
    assert_eq!(watermark.last_sequence, 2);
    assert_eq!(watermark.last_observed_at, at(10));
    assert_eq!(
        watermark.retired_boots,
        BTreeSet::from(["nomad-boot".into()])
    );
    let mut tracker =
        ComputeTracker::from_persisted(hosts(), "fresh", persisted.clone(), at(11)).unwrap();
    assert_eq!(tracker.persisted_state(), persisted);
    assert_eq!(
        tracker.record_sample(current.clone(), at(11)),
        Err(ComputeError::StaleSequence)
    );
    for (boot, seconds) in [("unknown-older-boot", 8), ("nomad-boot", 11)] {
        let mut delayed = sample("nomad", 99, seconds);
        delayed.boot_id = boot.into();
        assert_eq!(
            tracker.record_sample(delayed, at(11)),
            Err(ComputeError::StaleBoot)
        );
    }
    assert_eq!(
        tracker.snapshot(at(11)).hosts["nomad"].state,
        HostAvailability::Unknown
    );
    current.sequence += 1;
    current.observed_at = at(12);
    tracker.record_sample(current, at(12)).unwrap();
    assert_eq!(tracker.persisted_state().claims, [claim]);
    assert_eq!(
        tracker.snapshot(at(12)).hosts["nomad"].state,
        HostAvailability::Busy
    );
}

#[test]
fn deadlines_are_exact_and_time_only_changes_bump_revision_once() {
    let mut tracker = ComputeTracker::new(hosts(), "deadlines");
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let mut req = request("lease");
    req.ttl_secs = 1;
    let claim = grant(&mut tracker, req, "lease", 0);
    assert_eq!(claim.expires_at, at(15));
    tracker
        .enqueue_waiter(request("waiter"), OWNER, at(10), at(0))
        .unwrap();
    assert_eq!(tracker.next_deadline(), Some(at(10)));
    assert!(tracker.advance(at(10)));
    assert_eq!(tracker.next_deadline(), Some(at(15)));
    assert!(tracker.advance(at(15)));
    assert_eq!(tracker.next_deadline(), Some(at(30)));
    assert_eq!(
        tracker.persisted_state().claims[0].reconciling_since,
        Some(at(15))
    );
    let revision = tracker.snapshot(at(29)).revision;
    assert!(!tracker.advance(at(29)));
    assert_eq!(
        tracker.snapshot(at(30)).hosts["nomad"].state,
        HostAvailability::Busy
    );
    assert!(tracker.advance(at(30)));
    let snapshot = tracker.snapshot_with_preference(at(30), &["nomad", "atlas"]);
    assert_eq!(snapshot.revision, revision + 1);
    assert!(snapshot.hosts["nomad"].stale);
    assert_eq!(snapshot.recommended_host, None);
    assert_eq!(tracker.next_deadline(), None);
    assert!(!tracker.advance(at(30)));
}

#[test]
fn stale_or_pre_reconciliation_samples_cannot_clear_a_claim() {
    let mut tracker = ComputeTracker::new(hosts(), "evidence");
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let mut req = request("unknown-unit");
    req.ttl_secs = 15;
    let claim = grant(&mut tracker, req, "lease", 0);
    let mut tracker =
        ComputeTracker::from_persisted(hosts(), "restored", tracker.persisted_state(), at(15))
            .unwrap();
    assert_eq!(
        tracker.heartbeat("lease", running(&claim, "too-late"), OWNER, at(15)),
        Err(ComputeError::ClaimFenced)
    );
    tracker
        .record_sample(sample("nomad", 1, 15), at(15))
        .unwrap();
    assert_eq!(
        tracker.snapshot(at(15)).hosts["nomad"].state,
        HostAvailability::Busy
    );
    let mut expired = sample("nomad", 2, 16);
    expired.ttl_secs = 1;
    tracker.record_sample(expired, at(17)).unwrap();
    assert_eq!(tracker.persisted_state().claims.len(), 1);
    let mut with_workload = sample("nomad", 3, 18);
    with_workload.active_workloads.push(ObservedWorkload {
        kind: "nixOffload".into(),
        name: "unknown-unit.service".into(),
        project: None,
        memory_current_bytes: None,
    });
    tracker.record_sample(with_workload, at(18)).unwrap();
    assert_eq!(tracker.persisted_state().claims.len(), 1);
    tracker
        .record_sample(sample("nomad", 4, 19), at(19))
        .unwrap();
    assert!(tracker.persisted_state().claims.is_empty());
}

#[test]
fn claim_actions_require_the_principal_not_the_public_holder_label() {
    let mut tracker = ComputeTracker::new(hosts(), "ownership");
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let req = request("lease");
    assert!(matches!(
        tracker.try_claim(req.clone(), "lease", " ", at(0)),
        Err(ComputeError::InvalidRequest(_))
    ));
    assert!(matches!(
        tracker.enqueue_waiter(req.clone(), "", at(20), at(0)),
        Err(ComputeError::InvalidRequest(_))
    ));
    let claim = grant(&mut tracker, req.clone(), "lease", 0);
    assert_eq!(claim.owner_principal, OWNER);
    assert_eq!(
        tracker.try_claim(req.clone(), "retry", OTHER, at(1)),
        Err(ComputeError::HolderMismatch)
    );
    assert_eq!(tracker.is_eligible_for_request(&req, OTHER, at(1)), None);
    assert_eq!(
        tracker
            .is_eligible_for_request(&req, OWNER, at(1))
            .as_deref(),
        Some("nomad")
    );
    assert_eq!(
        tracker.heartbeat("lease", running(&claim, "unit"), OTHER, at(1)),
        Err(ComputeError::HolderMismatch)
    );
    let release = ReleaseRequest {
        holder: claim.holder.clone(),
        unit_confirmed_stopped: true,
        never_started: false,
    };
    assert_eq!(
        tracker.release("lease", release, OTHER, at(1)),
        Err(ComputeError::HolderMismatch)
    );
    let mut heartbeat = running(&claim, "unit");
    heartbeat.holder = "different display label".into();
    heartbeat.ttl_secs = Some(u64::MAX);
    assert_eq!(
        tracker
            .heartbeat("lease", heartbeat, OWNER, at(1))
            .unwrap()
            .expires_at,
        at(301)
    );
    assert!(matches!(
        tracker.heartbeat("lease", running(&claim, "different"), OWNER, at(2)),
        Err(ComputeError::InvalidRequest(_))
    ));
    let mut regressing = running(&claim, "unit");
    regressing.stage = Some(ClaimStage::PendingStart);
    assert!(matches!(
        tracker.heartbeat("lease", regressing, OWNER, at(2)),
        Err(ComputeError::InvalidRequest(_))
    ));
    let mut release = ReleaseRequest {
        holder: claim.holder.clone(),
        unit_confirmed_stopped: false,
        never_started: false,
    };
    assert_eq!(
        tracker
            .release("lease", release.clone(), OWNER, at(2))
            .unwrap(),
        ReleaseOutcome::Reconciling
    );
    assert_eq!(
        tracker.heartbeat("lease", running(&claim, "unit"), OWNER, at(3)),
        Err(ComputeError::ClaimFenced)
    );
    release.unit_confirmed_stopped = true;
    assert_eq!(
        tracker.release("lease", release, OWNER, at(3)).unwrap(),
        ReleaseOutcome::Released
    );
    let pending = grant(&mut tracker, only("nomad", "pending"), "pending", 4);
    assert_eq!(pending.stage, ClaimStage::PendingStart);
    assert_eq!(
        tracker
            .release(
                "pending",
                ReleaseRequest {
                    holder: pending.holder.clone(),
                    unit_confirmed_stopped: true,
                    never_started: false,
                },
                OWNER,
                at(5),
            )
            .unwrap(),
        ReleaseOutcome::Reconciling
    );
}

#[test]
fn omitted_workload_inventory_complete_defaults_false_and_cannot_clear_reconciling() {
    let mut tracker = ComputeTracker::new(hosts(), "legacy-inv");
    tracker.record_sample(sample("atlas", 0, 0), at(0)).unwrap();
    let claim = grant(&mut tracker, only("atlas", "work"), "work", 0);
    assert_eq!(
        tracker
            .release(
                "work",
                ReleaseRequest {
                    holder: claim.holder,
                    unit_confirmed_stopped: false,
                    never_started: false,
                },
                OWNER,
                at(1),
            )
            .unwrap(),
        ReleaseOutcome::Reconciling
    );
    let mut legacy = serde_json::to_value(sample("atlas", 1, 2)).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("workloadInventoryComplete");
    let decoded: HostTelemetrySample = serde_json::from_value(legacy).unwrap();
    assert!(!decoded.workload_inventory_complete);
    tracker.record_sample(decoded, at(2)).unwrap();
    assert_eq!(
        tracker.snapshot(at(2)).hosts["atlas"].active_claims.len(),
        1
    );
    assert_eq!(
        tracker.snapshot(at(2)).hosts["atlas"].state,
        HostAvailability::Busy
    );
}

#[test]
fn wire_defaults_round_trip_and_schema_versions_fail_closed() {
    fn check<T: Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq>(value: &T) {
        let mut json = serde_json::to_value(value).unwrap();
        assert_eq!(json["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(&serde_json::from_value::<T>(json.clone()).unwrap(), value);
        json["schemaVersion"] = json!(SCHEMA_VERSION + 1);
        assert!(
            serde_json::from_value::<T>(json)
                .unwrap_err()
                .to_string()
                .contains("unsupported compute schema version")
        );
    }
    let mut req: PlacementRequest = serde_json::from_value(json!({
        "schemaVersion": 1, "idempotencyKey": "a", "holder": "worker",
        "kind": "adjacentRun", "project": "phemus", "requiredBytes": 1
    }))
    .unwrap();
    assert!(req.prefer.is_empty());
    assert_eq!(req.ttl_secs, 90);
    check(&req);
    let mut tracker = ComputeTracker::new(hosts(), "wire");
    let mut sample = sample("nomad", 0, 0);
    assert!(sample.workload_inventory_complete);
    assert_eq!(sample.contract_memory_max_bytes, None);
    check(&sample);
    tracker.record_sample(sample.clone(), at(0)).unwrap();
    req.prefer = vec!["nomad".into(), "atlas".into()];
    let claim = grant(&mut tracker, req.clone(), "id", 0);
    check(&claim);
    let mut old_claim = serde_json::to_value(&claim).unwrap();
    old_claim.as_object_mut().unwrap().remove("startedAt");
    assert_eq!(
        serde_json::from_value::<ComputeClaim>(old_claim.clone())
            .unwrap()
            .started_at,
        None
    );
    old_claim["ownerPrincipal"] = json!(" ");
    assert!(serde_json::from_value::<ComputeClaim>(old_claim).is_err());
    let packed = tracker.persisted_state();
    check(&packed);
    let mut old_state = serde_json::to_value(&packed).unwrap();
    old_state.as_object_mut().unwrap().remove("hostWatermarks");
    assert!(
        serde_json::from_value::<PersistedState>(old_state)
            .unwrap()
            .host_watermarks
            .is_empty()
    );
    let snap = tracker.snapshot(at(0));
    check(&snap);
    let snap_json = serde_json::to_value(snap).unwrap();
    assert_eq!(
        snap_json["hosts"]["nomad"]["activeClaims"][0]["stage"],
        "pendingStart"
    );
    assert_eq!(
        snap_json["hosts"]["nomad"]["activeClaims"][0]["kind"],
        "adjacentRun"
    );
    assert_eq!(
        snap_json["generatedAt"],
        at(0).to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    );
    assert!(snap_json["hosts"]["atlas"]["remainingHeadroomBytes"].is_null());
    sample.schema_version = 2;
    assert_eq!(
        tracker.record_sample(sample, at(0)),
        Err(ComputeError::UnsupportedSchemaVersion(2))
    );
    req.schema_version = 2;
    assert_eq!(
        tracker.try_claim(req, "ignored", OWNER, at(0)),
        Err(ComputeError::UnsupportedSchemaVersion(2))
    );
    let mut bad = packed.clone();
    bad.claims[0].schema_version = 2;
    assert!(matches!(
        ComputeTracker::from_persisted(hosts(), "wire", bad, at(0)),
        Err(ComputeError::UnsupportedSchemaVersion(2))
    ));
    let mut bad = packed;
    bad.next_generation = claim.generation;
    assert!(matches!(
        ComputeTracker::from_persisted(hosts(), "wire", bad, at(0)),
        Err(ComputeError::InvalidPersistedState(_))
    ));
}

#[test]
fn invalid_samples_do_not_replace_valid_telemetry() {
    let mut tracker = ComputeTracker::new(hosts(), "validate");
    tracker.record_sample(sample("nomad", 0, 0), at(0)).unwrap();
    let valid = sample("nomad", 1, 1);
    for invalid in [
        HostTelemetrySample {
            boot_id: " ".into(),
            ..valid.clone()
        },
        HostTelemetrySample {
            ttl_secs: 0,
            ..valid.clone()
        },
        HostTelemetrySample {
            ttl_secs: 301,
            ..valid.clone()
        },
        HostTelemetrySample {
            memory_full_avg10: Some(f64::NAN),
            ..valid.clone()
        },
        HostTelemetrySample {
            max_memory_full_psi: Some(f64::INFINITY),
            ..valid.clone()
        },
        HostTelemetrySample {
            role: HostRole::Primary,
            ..valid.clone()
        },
        HostTelemetrySample {
            observed_at: at(2),
            ..valid.clone()
        },
    ] {
        assert!(matches!(
            tracker.record_sample(invalid, at(1)),
            Err(ComputeError::InvalidSample(_))
        ));
    }
    let unknown = HostTelemetrySample {
        host: "unknown".into(),
        ..valid.clone()
    };
    assert_eq!(
        tracker.record_sample(unknown, at(1)),
        Err(ComputeError::UnknownHost("unknown".into()))
    );
    assert_eq!(tracker.snapshot(at(1)).revision, 1);
    assert!(tracker.record_sample(valid, at(1)).unwrap());
}
