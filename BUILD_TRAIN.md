# Shared native construction

Fleetix supplies a builder-local coordinator for independent deployment attempts.
When a second admitted request joins, ready dependencies shared by both requests
gain priority. Running work is never preempted, bounded aging prevents starvation,
and each request becomes ready as soon as its own exact outputs are valid.

## Features and ownership

| Cargo feature | Capability |
| --- | --- |
| `build-train-runtime` | Private coordinator, client, durable journal and preparation receipts |
| `build-train-native` | Native Nix backend, connection/service contracts and policy identity |
| `build-train-cli` | Typed standalone lifecycle commands; combine with `cli` for the `fleetix` executable |

The production `fleetixCrate` package enables `cli,build-train-cli`. Pure scheduler
users can disable default features. Native graph discovery, substitution and
realization use published `nix-manager-core`; no Toolbelt or Canix checkout is
required.

The caller freezes and roots its source, evaluates exact derivations, performs
live preflight and host admission, publishes/transfers outputs, and owns target
activation and its execution lease. Successful construction is not deployment
acceptance. The coordinator's `MemoryMax` limits coordinator-side processes;
the external admission policy controls Nix daemon builders.

## Builder service

Import `fleetix.nixosModules.build-train` and configure an existing operator:

```nix
fleetix.services.buildTrain = {
  enable = true;
  user = "operator";
  builder = "builder";
  admissionContract = qualifiedHostAdmissionIdentity;
  workers = 2;
};
```

The flake module supplies its production package by default. Direct imports of
`modules/build-train.nix` must supply `package`. The socket accepts the service
UID only; root/operator routing and external resource admission remain explicit.

Defaults preserve existing deployments:

- service: `fleetix-build-train.service`;
- socket: `/run/fleetix-train/coordinator.sock`;
- durable state: `/var/lib/fleetix-train`;
- contracts: `/etc/fleetix-train/connection.json` and `service.json`;
- direct roots: `/nix/var/nix/gcroots/per-user/<operator>/fleetix-train`.

`runtimeDirectory`, `stateDirectory` and `gcRoots` configure ownership locations.
The NixOS service preserves its private runtime directory on stop so the socket
lease inode and operator-owned parent survive restarts. Reboot clears `/run`;
start and stop the service to provision that namespace again.
Changing any coordinator configuration restarts the unit. A changed policy then
discards the foreign journal instead of failing closed.

### Operator user service

Set `fleetix.services.buildTrain.serviceScope = "user"` to run the same coordinator
under the operator's user manager. The default is `"system"`; existing system-mode
policy identities are unchanged. User mode enables lingering for the selected
operator and restricts the global user unit with `ConditionUser`. Routine control
then uses `systemctl --user start|stop|restart fleetix-build-train.service` and
`journalctl --user -u fleetix-build-train.service`, without sudo.

Socket, journal, preparation and GC-root paths remain unchanged. System tmpfiles
provisions their private operator-owned directories, including `/run` after each
boot. The user unit does not use `RuntimeDirectory` or `StateDirectory`: stopping
it must not remove recovery state or replace lock anchors. Private temporary
directories use user namespaces, so user mode requires `security.allowUserNamespaces`.
Nix clients connect to the system daemon socket; ordering against a user-manager
`nix-daemon.service` would not order the system daemon.

Changing manager scope changes policy; activate the new configuration and restart
the selected unit. The coordinator discards the old journal, so no offline
handover is needed. The initial handoff still needs permission to stop the
previous unit and activate the replacement configuration. Never start both
managers against one journal.

User mode retains the coordinator memory ceiling and stop/retention semantics, but
does not move Nix daemon builders or grant host-resource authority. Consumers must
qualify the new cgroup ancestors, aggregate host budget, CPU/IO/swap policy and
network routing before enabling it; a per-user ceiling alone does not preserve a
system-slice aggregate budget. Keep those effective policies in `admissionContract`.

## Request lifecycle

Every client uses the selected builder's connection contract. A second request
connects to that same coordinator, while retaining its own attempt and source.
There is no manually named train or request merging.

The caller supplies a JSON `Request`, using exact store identities:

```json
{
  "attempt": "deployment-attempt-unique-id",
  "target": "host-a",
  "source": "/nix/store/…-source#nixosConfigurations.host-a",
  "roots": ["/nix/store/…-nixos-system-host-a.drv^out"],
  "activates": true
}
```

The ellipses above are placeholders for actual immutable store paths. Dynamic
and floating outputs are unsupported. Restoration-only work cannot fall back to
compilation. Sibling outputs of one derivation cannot compile concurrently.

```text
fleetix build-train register --connection /etc/fleetix-train/connection.json --request request.json
```

Registration is durable and held: it retains source/derivation evidence but has
no scheduling priority and cannot dispatch. Side-effect-free native source and
derivation/named-output validation precedes durable intake; malformed identities
create neither a journaled request nor a root namespace. Valid intake is journaled
before its initial roots are created. The full ownership graph is journaled
before dependency/output roots are created; preparation stays held until retention
finishes. Cancellation, timeout and partial retention failure preserve enough
evidence for retirement after the planner exits, including across restart.

`register --wait-seconds N` bounds delivery, planner queueing and preparation
(default 86400, range 1–86400). Select a wait that covers the service's planning
deadline and expected queueing. Timeout, SIGINT or SIGTERM only detaches: inspect
the same attempt with `status` before retrying; repeat `register` with the same
frozen request to await preparation. Library clients use `Client::register_for`
for the same explicit, interruptible deadline. The coordinator prunes fully
disconnected registration sockets while preserving their durable requests, so
detachment releases waiting-client capacity for reattachment and controls.

After the caller's live checks pass:

```text
fleetix build-train admit --connection /etc/fleetix-train/connection.json --attempt deployment-attempt-unique-id
fleetix build-train wait --connection /etc/fleetix-train/connection.json --attempt deployment-attempt-unique-id --wait-seconds 3600
```

`wait` observes only that request. Timeout, SIGINT or SIGTERM detaches; it does
not cancel construction. Completion failures and cancellation return nonzero.
`status` returns terminal evidence successfully, including failure evidence.

Explicit `cancel` removes only that request's interests; already-running shared
work finishes. Explicit `retry` clears only the invoking request's terminal
failure and returns it to held admission. Rerun live checks before `admit`.
`retire` releases only its train-owned roots and is idempotent: an attempt the
coordinator no longer knows is a successful no-op. Caller-owned deployment roots
remain the caller's responsibility.

Each request roots only its source and requested derivations under
`requests/<sha256(attempt)>/`; every planned derivation is in their closure.
Outputs share `outputs/` and are rooted once they exist: every live named
output of a derivation is rooted just before its build and again when the
store confirms it. A reservation dangles while its build runs, and after a
failed build until its node retires. Startup reserves the outputs of
dispatches it finds interrupted, and a failed completion reroots every
existing live output, since a partial substitution can fetch other nodes'
outputs. No root points at an output that was merely planned, so
cleaners that delete dangling roots (`nh clean`) cannot strip a pending
request's retention. Startup and every `retire` prune request directories and
output roots that no journaled request or node still owns, including
directories whose request left the journal without a release and per-path roots
written by older coordinators. Non-root entries are left for inspection.
Retirement writes an archive only while its root release is outstanding; the
archive is removed once the release is durable, so history does not grow.

## Activation and recovery

`drain --attempt ID --wait-seconds N` establishes the durable activation fence
before waiting for workers. Timeout or uncertain delivery requires inspecting
status; it must never be treated as fence release. `authorize-activation` checks
construction-side readiness but does not acquire the host execution lease or
activate anything. `release-fence --token TOKEN` is idempotent: a token which no
longer holds the fence is already released, so a retried release succeeds.

A fence records its deadline (`fenceSeconds`, default 3600). Every scheduling
tick and startup drops an expired fence and dispatch resumes, so a dead owner
cannot block builders forever. The owning attempt refreshes the deadline while
it still holds the fence; another attempt is refused (`builder fenced by …`).
Choose a deadline above the expected drain and activation window.

The coordinator is crash-only. The journal is a cache of scheduling state bound
to one policy, not recovery evidence: Nix already deduplicates store outputs.
At startup a journal with another protocol version, policy or aging value (or
one that cannot be parsed), or an interrupted handover marker, is discarded. One
stderr line records the old version, policy, request count and fence. The
coordinator removes `train.json`, `rollover.json`, `archive/` and the legacy
`rollovers/` directory, starts an empty train, and prunes every coordinator-owned
GC root that the fresh journal does not own. Caller-owned `attempts/` roots and
`preparation/` are untouched. A policy change therefore needs only an ordinary
unit restart; no drain, fence token or offline handover is required.

Library consumers use `build_train::native::{Connection, Service, serve,
policy_identity}` and `build_train::runtime::Client` directly. A client reply
from another policy or protocol fails with `runtime::POLICY_CHANGED`; use
`runtime::is_policy_changed` to separate that from transport errors. Frontends
can use `build_train::cli::execute` for typed results and their own presentation.
Protocol and positional policy serialization remain version 2; a journal without
a fence deadline is treated as already expired.
