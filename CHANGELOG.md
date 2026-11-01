# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.0] - 2026-11-01

### Changed

- **Breaking:** `DeviceType` gains the `Worker` variant; exhaustive matches
  need a new arm.

### Added

- Add the `worker` device type (`DeviceType::Worker`) for headless compute
  hosts that keep only ephemeral data.
- Add free-form `Host.tags` for consumer-defined host classes (#23).

## [0.9.0] - 2026-10-31

### Added

- Add the optional `forge` feature and `fleetix::forge`, which lists Forgejo,
  GitHub and GitLab namespace repositories as normalized `Repository` records
  (size in bytes, visibility, fork, mirror, archived, empty, last update).
  `Filter` selects repositories by those fields, `pull_request_totals` counts
  Forgejo pull requests in any state, and `quota` returns the Forgejo user
  quota. Callers supply tokens.

## [0.8.0] - 2026-10-27

### Added

- `ProxyAction.fallbackEndpoints` lists ordered endpoints tried after
  `endpoint` when it is unavailable. Each is validated like the primary;
  repeats fail as `site.route_duplicate_fallback_endpoint` and differing
  `transport`/`tlsServerName` as `site.route_fallback_transport_mismatch`.
  The Nix `services.resolveProxyEndpoints` helper resolves the ordered set.
- `"lan"` `IngressScope` and `HttpAccess` values for private LAN ingress. LAN
  sites skip managed CNAME synthesis and public probes, and managed DNS fails
  as `site.lan_managed_dns`.
- Add `fleetix::supply`, which merges versioned `fleetix.supply-facts.v1`
  documents (nodes, edges, capability rows and errors) from independent
  producers into one graph. `gaps` reports each uncovered usage with its
  blocker as `fleetix.supply-gaps.v1`, and `walk` follows dependencies forward
  or backward as `fleetix.supply-path.v1`. The module needs only serde, so it
  builds without the `cli` or `pkl` features. [SUPPLY.md](SUPPLY.md)
  specifies the contract and a recommended blocker vocabulary.
- Add an optional topology `supply` section with `RunnerPool{forgeHost,
  labels, service?, metered}`, `BinaryCache{url, service?}` and
  `Budget{pools, minutesPerMonth}`. All default to empty, so existing
  topologies are unchanged. Validation adds `supply.unknown_service`,
  `supply.duplicate_runner_label` and `supply.unknown_budget_pool`. Supply facts
  join runner and cache nodes against it and report `unknown-runner` and
  `unknown-cache` (R6).
- Add `fleetix supply gaps|path|graph`. `gaps` groups by blocker or
  repository. `path` walks forward, or backward with `--reverse` (for example,
  who pins a revision). `graph` dumps the merged graph. Each `--facts` file
  holds one document or an array. `--topology` enables the runner and cache
  join, and `--max-age-days` warns about stale facts. Human output is Markdown
  tables; `--format json` prints the versioned report.
- Add `fleetix supply budget`, a `fleetix.supply-budget.v1` forecast per
  topology budget. It sums observed workflow minutes scaled to 30 days, plus
  planned updater qualification runs on metered pools. It also ranks reduction
  candidates (`duplicate-trigger`, `heavy-on-pr`, `failure-waste`,
  `timeout-waste`) by estimated savings.

### Changed

- **Breaking:** `IngressScope::Lan`, `HttpAccess::Lan` and the
  `HttpAction::Proxy::fallback_endpoints` field break exhaustive Rust matches
  and struct-literal construction.
- Run the CI, Nix build and prebuild workflows on push only for `trunk` and
  the simit updater's `simit/update/**` candidate refs. Pull request heads are
  validated once, by the `pull_request` run, which roughly halves the jobs per
  push to a PR branch. Trunk pushes still publish to Attic.

### Fixed

- Declare `BodyAssertion.operator` through a `BodyOperator` typealias so Pklr 5
  evaluates assigned operators; required inline string-literal unions fail
  there (jdx/pklr#332).

## [0.7.0] - 2026-10-18

### Changed

- Require pklx 0.2.1 and Pklr 5.2 for imported schema constraints.
  Evaluation options now accept a Ureq
  `agent` instead of a Reqwest `client`. HTTP rewrites and the CLI proxy option
  retain their existing interfaces. Evaluation runs on Tokio's blocking pool.
- Trace binary file reads and environment enumeration in persistent sidecar
  cache receipts as well as text reads, imports and globs.
- Use published Nix Manager Core 0.4.2 for the optional native build backend,
  sharing the current Forgejo and terminal dependency cohort with consumers.

### Fixed

- Remove the vulnerable ZIP 8 extraction implementation through ZIP 9.0.5
  and update Rustls to 0.23.46 for its four October advisories.
- Update rustls-webpki to 0.103.16 and webpki-root-certs to 1.0.10 for
  RUSTSEC-2026-0382 and RUSTSEC-2026-0383.

## [0.6.1] - 2026-10-08

### Added

- Add filtered frontier dispatch for caller-owned build placement. Excluded goals
  stay pending while dependency readiness, fairness, independent failures and
  named-output builder deduplication retain their existing semantics.

### Changed

- Generate bounded exact-source Nix qualification and independent workspace
  archive publishing with Simit 0.20.0. Candidate builds carry no publishing
  credential; protected publication verifies the signed source and releases the
  sidecar prerequisite before Fleetix.

### Fixed

- Declare the sidecar's existing README in its crates.io package metadata.
- Update the locked QUIC protocol dependency to address five upstream advisories
  and replace the yanked ChaCha20 dependency with its compatible patch release.

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

- Prune fully detached registration waiters while preparation continues, freeing
  per-attempt and global connection budgets without cancelling durable requests
  or dropping live clients that have only closed their write half.
- Validate native intake identities before journaling, so a rejected malformed
  request cannot become an unrecoverable startup-retention obligation. Enforce
  Nix's store-name length and leading-component rules at that same boundary.
- Explicitly release drained coordinator leases on graceful stop so temporary
  fork-inherited file descriptions cannot block the replacement coordinator.
- Reject non-normalized GC-root namespaces during NixOS option checking, before
  invalid tmpfiles paths can reach activation.
- Reject noncanonical source, derivation and output-root evidence before creating
  request namespaces; unsuccessful terminal registration reattachments now fail
  the standalone command while preserving status evidence.
- Pin the active service executable, connection, operator and unit resource
  policy with `retainedDeployment` while staging an offline policy change.
  Bind operator identity inside the version-2 admission component and reject
  in-place operator custody transfers.
- Validate and durably journal initial intake before retaining source/derivation
  roots. Invalid or unjournaled requests create no roots; partial initial
  retention failure remains a known, retirable terminal request.
- Preserve the private runtime directory when the NixOS service stops, retaining
  the operator-owned socket lease namespace required for offline rollover.
- Persist the full planning ownership graph before creating dependency/output
  roots, so cancelled, timed-out and partially failed preparation can be retired
  after restart without orphaning roots. Dispatch stays held through retention.
- Let standalone registration use an explicit, interruptible `--wait-seconds`
  deadline (default 86400) covering queueing and planning instead of truncating
  valid service preparation to five minutes. Expiry detaches without cancellation.
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

[Unreleased]: https://github.com/caniko/fleetix/compare/0.10.0...HEAD
[0.10.0]: https://github.com/caniko/fleetix/compare/0.9.0...0.10.0
[0.9.0]: https://github.com/caniko/fleetix/compare/0.8.0...0.9.0
[0.8.0]: https://github.com/caniko/fleetix/compare/0.7.0...0.8.0
[0.7.0]: https://github.com/caniko/fleetix/compare/0.6.0...0.7.0
[0.6.0]: https://github.com/caniko/fleetix/compare/0.5.2...0.6.0
[0.5.0]: https://github.com/caniko/fleetix/compare/0.4.0...0.5.0
