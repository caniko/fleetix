# Product boundaries

Fleetix is the generic fleet product. It exposes a Rust library and a standalone
CLI. Its target scope includes configurable deployment execution with a
conventional NixOS backend, as well as topology, validation, planning, export,
trust, and shared adapters.

Deployment execution is an extraction target, not an API shipped by the current
release. Existing `Deployment` topology data describes service
intent/ingress; operational plans need a separate versioned contract.

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
