# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-10-07

### Added

- Expose native shared construction directly from Fleetix, with optional native
  and CLI features, standalone held registration/admission/completion, activation
  fencing and recovery commands, and a builder-local NixOS module.
- Bound completion waits across socket delivery and polling. Timeout and signals
  detach without cancelling durable work; unsuccessful terminal waits fail the
  process while status retains their evidence.
- Add `fleetix-sidecar`, an independent Rust CLI and library in a Fleetix Cargo
  workspace. Generate Pkl-to-Nix files without running Nix; check exact output
  bytes without rewriting sidecars or cache entries.

### Changed

- Preserve version-2 service/policy/protocol contracts while moving the generic
  native adapter into Fleetix. Existing ownership locations remain defaults;
  deployers can explicitly select their private directories and root namespace.
- Coordinate workspace publication in dependency order through Simit, with both
  crates using version 0.6.0 so the sidecar prerequisite publishes before Fleetix.
- Move the existing renderer, dependency-aware cache and atomic sidecar writer
  into the shared crate. Preserve `fleetix::pkl_to_nix` and the Fleetix CLI so
  Canix's configuration commands retain the same implementation and output.
- Qualify both workspace members in the Rust Nix gates and expose the standalone
  `fleetix-sidecar` package. Registry rollout requires publishing that prerequisite
  before its consuming Fleetix release.

### Fixed

- Keep existing unbounded library waits interruptible during stalled socket
  delivery, so legacy clients detach promptly without cancelling construction.
- Reject unrepresentable drain durations before establishing an activation fence
  instead of panicking in the library frontend.

## [0.5.2] - 2026-10-07

### Fixed

- Add an explicit offline policy rollover for a stopped, drained coordinator with
  an exact retained fence and terminal requests. Preserve the old journal,
  supersession, failures and archives before root release and initialize the new
  policy only after retirement succeeds. Interrupted handovers block startup and
  can be retried with the same checksum-bound evidence.
- Allow old-policy status and retirement against completed historical archives,
  while rejecting old request intake, admission, retry and activation. Keep IPC
  and journal version 2, and prevent repeated rollover from resetting new work.

## [0.5.1] - 2026-10-06

### Fixed

- Keep a failed build-train request terminal when its output is materialized by
  another worker or observed during restart. Capture the original failure in the
  request journal before importing store evidence; only that request's explicit
  retry clears it and activation still requires caller admission.
- Preserve these failures when recovering existing version-2 journals that stored
  the error only in a shared node, without changing the journal or IPC version.

## [0.5.0] - 2026-10-06

- Add a dependency-aware shared-construction library and optional authenticated
  builder-local coordinator, with late joins, bounded fairness, independent
  request completion, exact-output sharing, restart reconciliation, cancellation,
  durable activation fences and exact preparation receipts. The implementation
  retains separate consumer gates for production Nix and performance acceptance.
- Prepare graphs on bounded independent planners, keep control commands responsive,
  and recover held preparation across restart. Count active requests separately
  from terminal history; explicit retirement archives evidence before releasing
  request-owned roots and preserves activation supersession tombstones.
- Introduce the construction API in a new minor after the topology-only `0.4.0`
  release, preserving protocol and journal version 2.
- Bound root-output evidence before admission and return explicit oversized-reply
  errors. Keep terminal diagnostics readable over IPC while retaining their full
  detail in the durable journal.

## [0.4.0] - 2026-10-02

### Fixed

- Reject unknown GPU inventory vendors and media vendors absent from the
  declared inventory, matching the Nix route validation contract.

### Added

- Independent rendering routes with stable PCI render-node identities, shared
  Pkl/Rust/Nix conformance fixtures, and purpose-specific Nix route projections.
- Installed-system management routes with independent public recovery addresses,
  declared SSH ports, shared-link candidates, and enrolled runtime host-key checks.
- Explicit DNS-only publication targets, static address projections, and
  validation against competing DDNS, Pages, and explicit address-record writers.

### Changed

- GPU media routes now require stable PCI aliases instead of probe-order DRM
  node numbers. GPU inventory and compute vocabulary are constrained in Pkl.
- Rust consumers using `default-features = false` must enable `pkl` for
  evaluation/export and trust patching, or `health` for live probes. The default
  CLI retains both; lightweight GPU identity consumers need neither.
- Rust `Gpu` struct literals must supply the new `render` field or use defaults.
- Rust struct literals for `Host`, `Domains`, and `HttpSite` must supply the new
  management/publication fields or use their available defaults. Serialized
  topology remains compatible through defaulted fields.

## [0.3.0] - 2026-09-30

### Added

- Backend-neutral service profiles with visibility, domain affiliation,
  lifecycle exclusions, owned endpoint/site references, and typed health checks.
- Gatus inventory selection, public diagnostic redaction, stable history keys,
  collision detection, and endpoint/site coverage reporting.
- A host-local health library for systemd units, recent successful jobs,
  consumer health contracts, and literal-loopback HTTP/TCP readiness checks.
  Publication uses runtime bearer credentials, bounded concurrency, and external
  endpoint heartbeats, without redirects or proxy-environment credential leakage.

## [0.2.0] - 2026-09-30

### Added

- Optional `mcp` library feature with shared harness adapters and managed-entry
  reconciliation, also available through the standalone CLI.

### Fixed

- Render Hermes MCP timeouts as integer seconds, rounding fractional seconds up
  to match its NixOS option type without shortening the configured timeout.

### Changed

- The portable topology schema and Rust model include SSH access policy and
  public virtual-address placement, allowing consumers to import one schema.
- Pkl evaluation and sidecar generation now follow the entrypoint's declared
  imports, regardless of its filename. Use named imports (`import "Host.pkl"
as H`) in aggregates. The automatic Canix-shaped flattening workaround is
  removed; custom sections and schema metadata are preserved in raw exports.
  Nix consumers can use the existing normalization facade for nullable fields.

## [0.1.0] - 2026-09-27

### Fixed

- Lock `rustls` at 0.23.45 to address RUSTSEC-2026-0285 before the first release.
- Reject DNS-dependent WireGuard endpoints and non-IPv4 NAT-PMP gateways before
  isolated namespace deployment
- Quote dynamic host keys in Pkl attribute syntax (`hub` -> `["hub"]`) in
  topology fixtures and Nix checks

### Added

- Local-access policies: opt-in `deployment.localAccess` schema, Rust bindings,
  validation, Nix helpers, and a NixOS reconciler that routes selected overlay
  traffic over a verified on-link path with SSH host-key-checked target probes
- Host-local generic VPN profiles with tagged WireGuard connection and NAT-PMP
  port-forwarding models, Rust and Nix accessors, and topology validation
- VPN-bound service endpoints in the Pkl schema and Rust topology model
- GitHub Actions verification and crates.io publication workflows generated by
  Simit, with fmt, Clippy, test, audit, docs, and package checks
- Cached Pkl-to-Nix Rust library API and standalone CLI, including dependency
  invalidation for modular topology inputs
- Registry-based `pklx` dependency so downstream Rust clients can install the
  `fleetix` crate from crates.io
- Pre-commit hooks for treefmt, cargo fmt, clippy, audit, MSRV, and nix flake
  check
- simit project metadata configuration

[Unreleased]: https://github.com/caniko/fleetix/compare/0.6.0...HEAD
[0.6.0]: https://github.com/caniko/fleetix/compare/0.5.2...0.6.0
[0.5.0]: https://github.com/caniko/fleetix/compare/0.4.0...0.5.0
