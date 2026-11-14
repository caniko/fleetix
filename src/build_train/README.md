# Shared construction trains

Fleetix's `build_train` module owns dependency scheduling for multiple independent
deployment requests. The optional `build-train-runtime` feature adds a Linux
builder-local coordinator, authenticated Unix-socket library client, durable
state, and exact-identity preparation receipts. Nix mechanics belong to the
backend; consumers retain fleet policy and deployment stages.

## Contract

- One coordinator has an immutable builder/store/admission policy. Joining with
  another protocol or policy fails closed. The initial operator domain is one
  effective UID, enforced by private directories/socket and Linux peer credentials.
- Deployment attempt, exact derivation/named-output goal, and train identity are
  distinct. Different frozen revisions may share identical compatible goals.
  Request data contains no credentials or arbitrary commands.
- `Register` joins and prepares the graph without admitting that request's work.
  `Admit` follows the caller's live safety stages. `Submit` is the combined API
  for callers that have already passed admission.
- Intake reserves its exact identity and activation order durably before planning.
  Protocol/journal version 2 prevents older coordinators from interpreting held
  unprepared requests as fully planned work. A journal from another version,
  policy or aging value, or one which cannot be parsed, is discarded at startup
  with its history; the journal is a cache of scheduling state and Nix already
  deduplicates store outputs. Dedicated bounded planners leave completions and
  control commands responsive. Planning deadlines fail only that request; a tardy
  planner retains its slot and roots until it exits. Retry then returns the
  request to held admission.
- Requests and replies fit a one-MiB frame. Planning rejects root-output evidence
  that cannot fit a reply before admission, keeping the terminal failure available
  through status. Wire diagnostics are bounded on UTF-8 boundaries; their full
  detail remains in the journal. Oversized retained replies return an explicit
  error rather than silently dropping a durable operation's acknowledgement.
- Capacity counts pending requests, including held/preparing requests, rather than
  terminal history. Explicit `Retire` releases request-owned roots and is
  idempotent; an unknown attempt is a successful no-op. Its archive exists only
  while the root release is outstanding and is removed once that is durable.
  Status of a retired attempt is available until then, and a retired attempt
  cannot be resubmitted while its archive remains. Retirement rejects live
  planners, running interests and fence owners. Release failure is retryable
  after restart. Other requests and caller-owned attempt roots retain their
  independent pins; backends without scoped release conservatively retain
  roots. Activation tombstones prevent retirement from promoting an older request.
- Only ready dependency-frontier goals dispatch. Shared-first prioritizes pending
  work; bounded aging eventually gives oldest-ready precedence. Ready roots win
  equal-priority ties so requests can finish independently. Running work continues.
- Restore-only parents prune build-only dependencies. The backend must guarantee
  restoration cannot fall through to source compilation. Static named multi-output
  graphs are supported; backend-unsupported graph forms return explicit errors.
  A join's plan changes only its needed outputs. Unselected siblings and pruned
  dependencies cannot reclassify another request's pending source work; unused
  plans may be replaced when an output is first needed.
- Named outputs of one derivation occupy at most one worker at a time. Completion
  rechecks the store for sibling outputs before scheduling another worker.
  Shared-build priority counts each admitted request once across those outputs;
  restoration retains exact-output priority.
- Disconnect detaches. Cancellation removes one request's interests. Running goals
  finish, including work still needed by other requests. Failures propagate only
  through required dependencies; independent branches continue. Retry is explicit
  and returns the request to held admission. Retrying a shared failure preserves
  other failed requests' terminal outcomes and capacity accounting until they
  explicitly retry, including across restart and successful shared reconstruction.
- Worker receipts are journaled before dispatch and use monotonically increasing
  generations. Restart reconciles store validity and retained GC roots before
  redispatch. Old acknowledgements cannot complete a new generation.
- Source and requested-derivation roots are retained before journal admission;
  outputs are rooted in a shared namespace only once they exist. Startup and
  retirement prune roots no journaled request or node owns. History is bounded:
  a startup discard removes `archive/` and legacy `rollovers/`, and retirement
  removes its archive once the release is durable.
- The newest submitted activating request supersedes older requests for the same
  target. Build-only requests do not supersede. Frontends authorize immediately
  before mutation under their shared target activation lease.

## Builder activation fence

`Fence` atomically closes dispatch while intake remains available. The caller
waits a bounded time for affected workers to drain, acquires the existing target
lease, activates, reconciles the backend/policy, and releases the exact fence
token. A timeout or failed activation retains the fence until the owner verifies
the outcome and explicitly releases it. `release-fence` is idempotent: a token
which no longer holds the fence is already released.

A fence records a deadline (`fenceSeconds`, default 3600). Every scheduling tick
and startup drops an expired fence and dispatch resumes, so a dead owner cannot
block builders forever; the same attempt refreshes the deadline while it still
holds the fence, and another attempt is refused. The drain deadline also bounds
socket connection, request delivery and status replies; slow or partial replies
cannot extend it. Zero wait permits a five-second fence round trip but never
waits for workers. An ambiguous fence reply requires inspection of that request's
existing fence before retrying.

## Crash-only policy changes

The journal is bound to one builder policy and is not recovery evidence. When the
service restarts with another protocol version, policy or aging value, an
interrupted online handover marker, or an unparsable journal, the coordinator logs
one line naming the old version, policy, request count and fence, then removes the
journal, `rollover.json`, `archive/` and `rollovers/`. It starts an empty train and
prunes every coordinator-owned request/output root the fresh journal does not own;
caller-owned `attempts/` roots and `preparation/` are untouched. A deployment that
changes policy therefore only needs an ordinary unit restart, and a stale fence
cannot block dispatch forever. Clients told their coordinator runs another policy
or protocol see `runtime::POLICY_CHANGED` (`runtime::is_policy_changed`).

## Preparation receipts

Evaluation keys include frozen source/input identity, attribute, evaluator and
settings. Cargo keys include immutable source, lockfile, toolchain, target,
features, exact command and effective environment. Per-key kernel leases own
tasks; only passed evidence is reusable. Skips, failures and interruptions are
distinct and are not stored as passes. Evaluation leases are acquired only
inside the work callback and released before Cargo or realization.

## Verification and rollout

```sh
cargo test --no-default-features --features build-train-cli --test build_train --test build_train_runtime
cargo clippy --no-default-features --features build-train-cli --all-targets -- -D warnings
treefmt --ci
```

Tests cover late joins changing actual coordinator worker order, independent
completion, restored-parent pruning, cancellation, failure isolation, exact
outputs/revisions, same-target supersession, bounded fairness, held admission,
durable and expiring fences, foreign-journal discard with history and root
pruning, bounded retirement archives, restart reconciliation, typed policy
mismatch and preparation reuse.

These fixtures establish scheduling and lifecycle behavior. Real Nix dispatch,
builder-host activation and performance require the integration gate. Compare
per-host candidate/deployment latency, total completion time, preparation work,
overhead, resource pressure and fairness against sequential rebuilds and ordinary
concurrent Nix requests with identical source/cache conditions. Nix already
reuses identical store outputs; no throughput gain is claimed from these tests.
