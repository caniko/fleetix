# Shared native construction

Fleetix supplies a builder-local coordinator for independent deployment attempts.
When a second admitted request joins, ready dependencies shared by both requests
gain priority. Running work is never preempted, bounded aging prevents starvation,
and each request becomes ready as soon as its own exact outputs are valid.

## Features and ownership

| Cargo feature | Capability |
| --- | --- |
| `build-train-runtime` | Private coordinator, client, durable journal and preparation receipts |
| `build-train-native` | Native Nix backend, connection/service contracts and policy rollover |
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
An in-place policy rollover requires those locations to remain unchanged.
The NixOS service preserves its private runtime directory on stop so offline
rollover can acquire the same socket lease as the operator. Reboot clears `/run`;
start and stop the service to provision that namespace before offline rollover.

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
no scheduling priority and cannot dispatch. The full ownership graph is journaled
before dependency/output roots are created; preparation stays held until retention
finishes. Cancellation, timeout and partial retention failure preserve enough
evidence for retirement after the planner exits, including across restart.

`register --wait-seconds N` bounds delivery, planner queueing and preparation
(default 86400, range 1–86400). Select a wait that covers the service's planning
deadline and expected queueing. Timeout, SIGINT or SIGTERM only detaches: inspect
the same attempt with `status` before retrying; repeat `register` with the same
frozen request to await preparation. Library clients use `Client::register_for`
for the same explicit, interruptible deadline.

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
`retire` archives terminal evidence before releasing only its train-owned roots.
Caller-owned deployment roots remain the caller's responsibility.

## Activation and recovery

`drain --attempt ID --wait-seconds N` establishes the durable activation fence
before waiting for workers. Timeout or uncertain delivery requires inspecting
status; it must never be treated as fence release. `authorize-activation` checks
construction-side readiness but does not acquire the host execution lease or
activate anything. `release-fence --token TOKEN` requires the exact retained
token and a verified owner outcome.

Policy changes require explicit staging of the retained deployment. Before
builder activation, retain and GC-root the original service
configuration and verify coordinator/Nix/admission/resource compatibility. Stage
policy changes with `fleetix.services.buildTrain.retainedDeployment` set to the
original `package`, `user` and immutable store `serviceConfig`. This keeps the
active unit (including crash restarts), `/etc/fleetix-train/service.json` and
`connection.json` on the original contract. The desired replacement is exposed
as `next-service.json` and `next-connection.json`; drain and cancel using the
active connection. The retained package and config remain in the system closure.

The module binds the operator account into the opaque admission component of
the version-2 policy. Historical contracts and their positional serialization
remain valid. An operator change is an ownership-domain migration, not an
in-place policy rollover: finish and retire the original deployment, then
provision a separate private namespace for the new account. Do not transfer
request-root or journal custody through `retainedDeployment`.

For an incompatible policy upgrade, hold the shared host activation lease, drain
the train, cancel pending requests individually and stop the old service. Then:

```text
fleetix build-train rollover --previous-config /nix/store/…-original-service.json --config /etc/fleetix-train/next-service.json --token exact-retained-token
```

After rollover succeeds, clear `retainedDeployment`, activate the configuration
under the same host lease, and start the service with the replacement policy.
Never restart the retained old-policy service against the replaced journal. Interrupted
rollover rejects startup and can be retried only with exact retained evidence.
Completed retries preserve replacement-policy work. Historical old-policy clients
can query `status` and `retire`; they cannot register, admit, retry or activate.
Protocol, journal and positional policy serialization remain version 2.

Library consumers use `build_train::native::{Connection, Service, serve,
policy_identity, rollover}` and `build_train::runtime::Client` directly. Frontends
can use `build_train::cli::execute` for typed results and their own presentation.
