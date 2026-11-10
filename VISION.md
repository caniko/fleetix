# Fleetix vision

Fleetix is the generic fleet product: a typed Pkl schema for fleet-wide host,
link, GPU, domain, and service topology, Rust bindings and a standalone CLI over
it, and NixOS and Home Manager modules that expose a generated topology sidecar
to a consumer flake. A consuming flake owns the real data; Fleetix owns the
shape, the bindings, the export path, and the reusable runtime pieces that
frontends build on.

## Problem

Every fleet ends up with its own ad-hoc map of hosts, addresses, VPN profiles, GPU
routes, and service placements, plus per-consumer copies of the code that reads
it. That duplicates validation, makes derived answers (which address do I use
for this host, which GPU belongs to this route) inconsistent between Nix and
Rust, and encourages a second deployment engine inside whichever consumer gets
impatient first.

## Principles

- Data and schema are separate. Fleetix ships the Pkl schema shape and a
  synthetic example; production topology stays in the consumer's flake, and
  generated sidecars are the consumer's artifacts.
- One derivation path for Nix and Rust. The sidecar converts Pkl to Nix, and the
  Rust crate reads the same topology, so both sides answer from the same data.
- Typed results, typed errors. Libraries return typed results; frontends own
  parsing and presentation.
- Independent of any one fleet. Fleetix takes explicit topology, source, state,
  tool, and policy inputs instead of assuming Canix's paths, users,
  repositories, or fleet names.
- Publish as a product, not as source distribution. Public Rust libraries are
  published to crates.io by Simit CI and consumed as versioned Cargo
  dependencies; flake inputs serve Nix modules and packaging. A consumer must
  never reuse a library by invoking another product's CLI.
- One generic engine. canix-toolbelt builds architecture conventions on
  Fleetix, and specialist engines (Nix management, secrets, Crossbow, Harbor)
  keep their ownership rather than being absorbed.
- MCP authorization is explicit. `Server::new` denies every invocation until the
  embedding application installs an authorizer, tool annotations and argument
  names are not authorization, and privileged tools must not get an
  unconditional allow policy.
- The optional MCP server is composable: consumers register their own typed
  tools beside Fleetix's, duplicate names are rejected, and handlers capture
  their own dependencies rather than invoking a downstream executable.

## Scope

The generic topology schema (hosts, links, domains, GPUs, services), accessors
and ordered address-policy resolution, validation, export and sidecar
generation, routing and management candidates, the build-train construction
slice, paired dev/prod service surfaces with exact-output promotion, MCP client
reconciliation, and the composable Rust MCP server. Agents use the shipped MCP
server to read a selected topology snapshot — hosts, links, and derived
addresses — over stdio instead of parsing Pkl files.

## Non-goals

Fleetix is not Canix, does not carry production fleet data, and is not a mandate
to absorb every neighbouring crate. General whole-host deployment execution
remains an extraction target: publication, target transfer, live resource
admission, and activation stay caller-owned until their deployment slices are
extracted. It does not promise zero-downtime or host-HA through service
surfaces.

## Direction

The recent work is the paired service-surface backend (independent dev and prod
service guests, immutable-output promotion, guarded recovery through a separate
`fleetix-surfaces` frontend) and generic topology routing that resolves ordered
candidates against a caller-supplied policy and probe. Immediately before that,
hosts gained free-form tags and a `worker` device type for headless compute.

## Place in the fleet

Canix pins Fleetix as a flake input and reads topology through its MCP tools;
`canix-toolbelt` builds its conventions on Fleetix; the `fleet` superproject
qualifies Fleetix revisions with the other components; and downstream products
such as harbor-cad consume its GPU route helpers.
