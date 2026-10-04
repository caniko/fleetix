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
  and returns the request to held admission.
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
