# Product boundaries

Fleetix is the generic fleet product. It exposes a Rust library and a standalone
CLI. Its target scope includes configurable deployment execution with a
conventional NixOS backend, as well as topology, validation, planning, export,
trust, and shared adapters.

Deployment execution is an extraction target, not an API shipped by the current
release. Existing `Deployment` topology data describes service
intent/ingress; operational plans need a separate versioned contract.

The optional `build-train-native` layer is the shared construction slice:
Fleetix composes published `nix-manager-core`, exposes explicit immutable service
and connection contracts, and supplies the standalone lifecycle and NixOS module.
See [BUILD_TRAIN.md](BUILD_TRAIN.md). Publication, target transfer, live resource
admission and activation remain caller-owned until their deployment slices are
extracted.

## Layering

- Fleetix operates independently of canix-toolbelt and Canix. It takes explicit
  topology, source, state, tool, and policy inputs rather than assuming their
  runtime paths, users, repositories, or fleet names.
- canix-toolbelt builds architecture conventions and reusable integrations on
  Fleetix. It must not contain a second generic deployment engine.
- Canix supplies its own fleet data and policy and calls the shared libraries.
- Existing specialist engines (Nix management, secrets, Crossbow, Harbor) retain
  their ownership; the product boundary is not a mandate to absorb every crate.

Public Rust libraries are published to crates.io by Simit CI and consumed using
versioned Cargo dependencies. Flake inputs serve Nix modules and packaging, not
Rust source distribution. Libraries return typed results and errors; frontends
own parsing and presentation. Do not implement library reuse by invoking another
product's CLI or importing consumer globals.

## Extraction acceptance

Extract vertical slices and migrate the consumer immediately after publication.
Preserve command/output/role contracts, attempt identities, checkpoint schemas,
GC roots, stage ordering, and resume behavior. Frontends activating the same host
must contend on the same kernel lock. Consumer-specific guards remain explicit
extensions with stable identities. Remote delegation requires a compatible
protocol and builder-first rollout.

Test independent synthetic consumers with registry dependencies and no Canix
checkout or Nix source injection. Library compilation and pure operations must
work without Nix; deployment may invoke explicitly configured Nix/SSH tools.
Maintain separate implementation and target status until all of these gates pass.

## Construction policy upgrades

The optional build-train runtime keeps protocol and journal version 2. Its policy
is an immutable execution/admission identity, not a setting that can be updated
under queued work. A changed-policy service rejects the old journal.

`build_train::runtime::rollover` is the explicit offline handover API. The caller
owns host activation coordination and stops the old service after draining its
exact activation fence. Pending requests must be cancelled individually; this
API never cancels other requests implicitly. Both configurations must keep the
same state and socket locations and select different policies.

Rollover takes both coordinator leases, saves a checksum-bound snapshot of the
old train and archives, and writes a durable interruption marker before retiring
terminal requests and releasing their backend-owned roots. The replacement empty
journal is published last. Startup refuses an interrupted handover; retry uses
the exact previous/next configurations and fence token. A completed retry cannot
erase new work. Neither lease anchors nor historical journals are deleted.

The replacement coordinator serves old-policy status/retirement only from
completed, root-released archives. It rejects old-policy intake, admission, retry
and activation. Historical failures, cancellation and supersession evidence stay
available without migrating old request identities to a new execution policy.
