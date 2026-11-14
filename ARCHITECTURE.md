# Product boundaries

Fleetix is the generic fleet product. It exposes a Rust library and a standalone
CLI. Its target scope includes configurable deployment execution with a
conventional NixOS backend, as well as topology, validation, planning, export,
trust, and shared adapters.

The opt-in [deployment slice](DEPLOYMENT.md) exposes recoverable NixOS and
standalone Home Manager activation. The service-surfaces backend implements
paired persistent NixOS service guests and exact-output promotion with a
separate versioned policy/journal. See [SERVICE_SURFACES.md](SERVICE_SURFACES.md).
Existing `Deployment` topology data continues to describe service intent/ingress,
not operation state.

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
under queued work. The coordinator is crash-only: a journal is a cache of
scheduling state bound to one policy, and Nix already deduplicates store outputs.

Startup with another protocol version, policy or aging value, an unparsable
journal, or an interrupted handover marker logs one line naming the old version,
policy, request count and fence, then removes the journal, marker, `archive/` and
legacy `rollovers/`. It starts an empty train and prunes every coordinator-owned
request/output GC root the fresh journal does not own. Caller-owned `attempts/`
roots and `preparation/` are untouched. A policy change therefore needs only an
ordinary unit restart; no drain, fence token, snapshot or offline handover exists.

An activation fence records its deadline and expires, so a dead owner cannot block
dispatch forever; the owning attempt refreshes it while still held. Retirement
archives survive only while their root release is outstanding.
