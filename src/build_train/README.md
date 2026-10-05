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
  unprepared requests as fully planned work. Version 1 journals fail closed and
  remain untouched; finish their requests with their original coordinator policy.
  Dedicated bounded planners leave completions and control commands responsive.
  Planning deadlines fail only that request; a tardy planner retains its slot and
  roots until it exits. Retry then returns the request to held admission.
- Capacity counts pending requests, including held/preparing requests, rather than
  terminal history. Explicit `Retire` archives terminal identity, outcome and graph
  before removing membership and releasing request-owned roots. Status remains
  available from the archive and a retired attempt cannot be reused. Retirement
  rejects live planners, running interests and fence owners. Release failure is
  retryable after restart. Other requests and caller-owned attempt roots retain
  their independent pins; backends without scoped release conservatively retain
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
- Source/derivation/output roots are retained before journal admission. There is
  currently no automatic root or history pruning; operators retain recovery
  evidence and clean it only after request owners release it.
- The newest submitted activating request supersedes older requests for the same
  target. Build-only requests do not supersede. Frontends authorize immediately
  before mutation under their shared target activation lease.

## Builder activation fence

`Fence` atomically closes dispatch while intake remains available. The caller
waits a bounded time for affected workers to drain, acquires the existing target
lease, activates, reconciles the backend/policy, and releases the exact fence
token. A timeout or failed activation retains the fence, including across service
restart, until the owner verifies the outcome and explicitly releases it.
The drain deadline also bounds socket connection, request delivery and status
replies; slow or partial replies cannot extend it. Zero wait permits a five-second
fence round trip but never waits for workers. An ambiguous fence reply requires
inspection of that request's existing fence before retrying.

Deployment integration keeps the old immutable coordinator alive while its
requests finish. A newly configured policy cannot attach to that older service.
An explicit policy upgrade must drain/resolve retained requests and migrate state;
incompatible state is never silently reused.

## Preparation receipts

Evaluation keys include frozen source/input identity, attribute, evaluator and
settings. Cargo keys include immutable source, lockfile, toolchain, target,
features, exact command and effective environment. Per-key kernel leases own
tasks; only passed evidence is reusable. Skips, failures and interruptions are
distinct and are not stored as passes. Evaluation leases are acquired only
inside the work callback and released before Cargo or realization.

## Verification and rollout

```sh
cargo test --no-default-features --features build-train-runtime --test build_train --test build_train_runtime
cargo clippy --no-default-features --features build-train-runtime --all-targets -- -D warnings
treefmt --ci
```

Tests cover late joins changing actual coordinator worker order, independent
completion, restored-parent pruning, cancellation, failure isolation, exact
outputs/revisions, same-target supersession, bounded fairness, held admission,
durable fences, restart reconciliation, policy mismatch and preparation reuse.

These fixtures establish scheduling and lifecycle behavior. Real Nix dispatch,
builder-host activation and performance require the integration gate. Compare
per-host candidate/deployment latency, total completion time, preparation work,
overhead, resource pressure and fairness against sequential rebuilds and ordinary
concurrent Nix requests with identical source/cache conditions. Nix already
reuses identical store outputs; no throughput gain is claimed from these tests.
