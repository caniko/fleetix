# Fleetix Grand Improvement Plan

## Outcome

Fleetix should become the authoritative, reusable contract for describing a
fleet topology once and consuming it consistently from Pkl, Rust, and Nix.
The completed system should have:

- one canonical topology schema;
- deterministic, parity-tested Rust and Nix projections;
- actionable validation before invalid topology reaches consumers;
- a small, typed, documented Rust library API;
- useful NixOS and Home Manager modules with explicit integration modes;
- atomic, automation-friendly CLI and export behavior;
- reproducible formatter, MSRV, feature, packaging, and release gates; and
- a clean ownership boundary between generic Fleetix behavior and canix policy.

This plan is dependency-ordered. A later phase may be explored early, but it
must not be declared complete while an earlier phase's exit gate is red.

## Current baseline

### Green (implemented in the current tranche)

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- default-feature `cargo check --all-targets`
- Rust documentation build
- `nix flake check --no-build --no-update-lock-file --accept-flake-config`
- targeted Nix package, helper, export, and module-integration checks
- 15 Rust unit tests, including IPv6 endpoint and versioned-archive coverage

### Remaining work

- `cargo package --allow-dirty --no-verify` remains intentionally blocked by
  the unpublished git dependency `pklx`; Fleetix is explicitly
  `publish = false` and CI enforces that policy.
- The schema-parity fixture matrix, native modular import resolver, generic
  Nix helper split, and downstream canix migration remain future tranches.
- The graph health audit reported dangling and collapsed extraction edges;
  graph centrality is useful for navigation, not proof of correctness.

### Confirmed design and correctness issues

- Pkl schema definitions are duplicated and already disagree on port types.
- Rust and Nix accessors have observable parity differences.
- Nix `zoneForHost` does not fall back from an empty `managedZones` list to
  `zones`.
- Validation says it checks dynamic hosts while iterating static-file
  services, and its suffix test is not label-boundary safe.
- `link_server` documents a multiple-server failure but returns the first
  server.
- The Home Manager module describes an `osConfig` mirror but does not wire it.
- Generic Fleetix Nix helpers expose canix-specific domain names and positional
  zone policy.
- Modular topology flattening is implemented with textual import, replacement,
  and brace parsing.

## Governing principles

1. **One owner per fact.** `Schema.pkl` owns portable topology shape. Fleetix
   owns generic validation and projections. Consumers own fleet data and local
   policy.
2. **Reject invalid state at boundaries.** Pkl evaluation, Rust loading, and
   Nix module evaluation should fail with the same actionable diagnosis.
3. **Parity is tested, not assumed.** Equivalent Rust and Nix helpers must run
   against shared fixtures and produce equivalent normalized results.
4. **Compatibility is explicit.** Public Rust APIs, flake outputs, Nix options,
   and sidecar formats receive deprecations or versioned migrations rather than
   silent breaks.
5. **Generated artifacts have named producers.** Every checked-in generated
   file documents its source, regeneration command, and validation gate.
6. **Generic policy lives at the highest reusable layer.** Canix-specific
   domains, defaults, deployment choices, and workload semantics remain in
   canix or its appropriate upstream project.
7. **Every tranche ends green.** Focused gates run first, followed by the full
   repository gate before the tranche is merged.

## Phase 0 — Restore a trustworthy baseline

### Work

- Add `formatter.<system>` for every maintained system and make `nix fmt --
  --check` authoritative locally and in CI.
- Add app metadata and descriptions for all public apps.
- Declare the CLI binary with `required-features = ["cli"]` and add explicit
  no-default/all-feature checks.
- Resolve the `pklx` packaging contract. The current tranche takes the safe
  second branch: Fleetix is explicitly non-publishable until a versioned
  `pklx` release exists; a later release tranche can switch the dependency and
  remove that guard after upstream publication is verified.
- Add an `msrv` Nix development shell/check using the actual Rust 1.88
  toolchain. Keep the declared MSRV aligned with the lowest compiler that can
  parse and build the resolved dependency graph.
- Add a pinned current-stable Rust 1.96 development shell/check so the
  implementation receives both minimum-version and current-stable coverage;
  do not raise the MSRV merely because a newer compiler is available.
- Add dual-license files matching `MIT OR Apache-2.0`.
- Ensure CI, pre-commit, and flake checks invoke the same command matrix.

### Exit gate

```bash
nix fmt -- --check .
nix develop -c cargo fmt --all -- --check
nix develop -c cargo clippy --all-targets --all-features -- -D warnings
nix develop -c cargo check --all-targets --all-features
nix develop -c cargo check --all-targets --no-default-features
nix develop -c cargo test --all-features
nix develop .#msrv -c cargo check --all-targets --all-features
nix develop .#stable -c cargo check --all-targets --all-features
nix develop -c cargo metadata --no-deps --format-version 1 \
  | sed -n '/^{/,$p' \
  | jq -e '.packages | length == 1 and .[0].publish == []'
nix flake check --no-update-lock-file --accept-flake-config
```

If `pklx` cannot yet be packaged, the phase may finish only with a documented
upstream blocker, an explicit `publish = false`, and a CI gate that enforces
that declared state instead of pretending publication works.

## Phase 1 — Establish the canonical contract

### Work

- Declare `lib/topology/Schema.pkl` the canonical portable schema.
- Stop hand-maintaining a second standalone schema in `Topology.pkl`:
  import the canonical schema or generate the compatibility file from it.
- Define a stable normalized topology representation for fixtures and sidecars.
- Add a schema/version field or equivalent compatibility envelope for exported
  sidecars and rkyv archives.
- Close fixed string sets in Pkl and Rust where appropriate, including link
  roles and device types.
- Align numeric types and ranges for ports, redirect statuses, and similar
  bounded fields.
- Decide and document optional-field semantics: absent, `null`, empty list,
  and default values must mean the same thing across languages.
- Remove duplicate facts such as host-level versus storage-level data roots.
- Record a compatibility policy for adding, renaming, defaulting, and removing
  fields.

### Shared fixture matrix

Create checked-in fixtures covering:

- a minimal valid topology;
- server/client and peer-to-peer links;
- IPv4 and IPv6 links;
- nested DNS zones and apex records;
- public, VPN-only, LAN-exposed, and unpublished services;
- redirects, static sites, internal services, and Codeberg Pages;
- malformed references, duplicate identities, invalid roles, invalid ports,
  invalid CIDRs, and hostname/zone boundary cases; and
- modular and standalone topology entrypoints.

Each valid fixture must evaluate through Pkl, deserialize through Rust, and
import through Nix. Invalid fixtures must fail at the earliest intended layer
with a stable diagnostic category.

### Exit gate

- No independently maintained duplicate schema remains.
- A schema-parity check compares the normalized Pkl, Rust, and Nix views.
- All current canix topology data is representable without consumer-specific
  fields being added to Fleetix.
- Sidecar/archive compatibility behavior is documented and tested.

## Phase 2 — Make validation authoritative

### Work

- Replace the compatibility `Vec<String>` views with typed, structured
  validation issues:
  severity, code, path, offending value, and message.
- Mark exported diagnostic enums `#[non_exhaustive]`.
- Validate link invariants:
  - link bindings reference declared links;
  - server cardinality matches link mode;
  - addresses parse and belong to the declared subnet;
  - addresses are unique per link;
  - public keys and ports satisfy the selected link mode; and
  - IPv4/IPv6 prefix behavior is correct.
- Validate host references:
  - build hosts;
  - direct-link peers;
  - reverse-proxy and internal-service targets; and
  - any future adapter-owned host reference.
- Validate DNS and service invariants:
  - label-boundary-safe zone membership;
  - dynamic-host, static-service, reverse-proxy, redirect, and Pages names;
  - duplicate service names and hostnames;
  - port and redirect-status ranges;
  - compatible publication flags; and
  - managed-zone ownership.
- Add an explicit validation policy for warnings versus errors.
- Make export fail before writing when validation errors exist; offer an
  explicit opt-out only if a real compatibility use case requires it.
- Add a machine-readable CLI validation format alongside human diagnostics.

The current tranche adds structured `ValidationIssue` records while retaining
the string vectors as a compatibility view; a future major release can remove
those duplicated fields after downstream consumers migrate.

### Exit gate

- Every validator branch has positive and negative tests.
- The confirmed dynamic-host/static-service and suffix-boundary bugs have
  regression tests.
- Equivalent invalid topology produces matching diagnostic categories from
  library and CLI entrypoints.
- No accessor silently repairs malformed topology with guessed `/24`, `/32`,
  empty strings, or first-match behavior.

## Phase 3 — Harden and clarify the Rust API

### Work

- Inventory everything reachable from the crate root and label each item as
  stable, provisional, deprecated, or internal.
- Split oversized modules by responsibility while preserving re-exports:
  - `topology/types`;
  - `topology/load`;
  - `validation`;
  - `projection/network`;
  - `projection/dns`;
  - `projection/services`;
  - `archive`; and
  - CLI-only export glue.
- Introduce domain types only where they prevent real invalid states, such as
  ports, CIDRs, host names, and service names. Preserve serde compatibility at
  the boundary.
- Keep `Host` as the aggregate root but separate identity, network, rebuild,
  hardware, storage, and user data cleanly.
- Reshape `ReverseProxyService` around focused target, publication, exposure,
  and proxy/TLS policies. Stage this behind compatibility conversion if it is
  a public breaking change.
- Replace broad `Option` results with typed `Result` where absence can mean
  malformed or ambiguous topology.
- Correct `link_server` cardinality behavior and IPv4/IPv6 allowed-IP logic.
- Give library boundaries named, source-preserving error enums; keep `miette`
  presentation in CLI glue.
- Audit `load_sync`: define behavior inside an existing Tokio runtime and
  avoid nested-runtime panics.
- Add `#[must_use]`, `#[non_exhaustive]`, common derives, and borrowed inputs
  where their semantics are sound.
- Define the rkyv archive compatibility and validation contract; never imply
  that an unversioned archive is stable across schema releases.

### Exit gate

- Public API documentation builds with warnings denied and doctests pass.
- Feature combinations compile independently.
- Every would-be breaking API change is listed in the changelog/migration
  guide.
- Public fallible operations preserve their source errors and identify paths
  or offending values.

## Phase 4 — Rebuild Nix integration around explicit contracts

### Work

- Define NixOS module modes explicitly:
  - disabled/no source;
  - generated-sidecar source; and
  - optionally direct generated topology supplied by a consumer.
- Use precise option types (`nullOr path`, `attrsOf`/submodules, ports, enums)
  and assertions for cross-option invariants.
- Decide whether `fleetix.source` is required when the module is imported or
  optional behind `fleetix.enable`; encode one coherent contract.
- Implement Home Manager integration honestly:
  - integrated mode mirrors `osConfig.fleetix.topology`;
  - standalone mode requires an explicit source/value or stays disabled; and
  - tests cover both modes.
- Fix zone fallback semantics and audit every Nix/Rust accessor pair for
  behavioral parity.
- Remove canix-specific names and positional domain assumptions from Fleetix
  (`tartanogluDomain`, `candeeDomain`, `syndbDomain`, and implicit secondary
  zone policy).
- Keep generic helpers such as longest-zone match, host-domain construction,
  explicit address policies, and service projections in Fleetix.
- Move concrete canix domain aliases, `wg-home` conventions, default ports,
  and workload adapters to canix unless they are parameterized generic
  adapters.
- Split `lib/default.nix` into cohesive internal files while keeping the
  public `fleetix.lib` facade and output names stable.

### Exit gate

- Minimal NixOS, integrated Home Manager, and standalone Home Manager fixtures
  evaluate successfully.
- Rust/Nix parity tests cover all public helper pairs.
- Fleetix contains no canix-owned domains or fleet-specific positional policy.
- Existing public flake output names remain available, with aliases and
  warnings for any staged replacement.

## Phase 5 — Replace brittle export and CLI plumbing

### Work

- Prefer native Pkl evaluation/import resolution over textual flattening.
- If a compatibility flattener must remain temporarily, give it an explicit
  limited grammar and adversarial tests for comments, strings containing
  braces, aliases, duplicate imports, malformed sections, and nested paths.
- Consolidate `eval`, `export`, and `pkl-to-nix` around one Rust export engine
  so normalization and error behavior cannot drift between shell and Rust.
- Replace direct destination writes with same-directory atomic writes,
  permission preservation, flush/sync as appropriate, and rename.
- Use safe temporary-file ownership and cleanup rather than timestamp/PID file
  naming.
- Factor repeated HTTP evaluator arguments into a Clap `Args` structure.
- Return normal errors from library/CLI dispatch rather than calling
  `process::exit` below `main`.
- Define stable CLI exit codes for usage, evaluation, validation, and I/O
  failures.
- Add `--format human|json` to `validate`, `show`, and `links` where automation
  benefits.
- Make missing requested hosts or links return a nonzero not-found result.
- Keep intentional command output on stdout and diagnostics on stderr; add
  structured tracing only for diagnostics that need verbosity controls.

### Exit gate

- Interrupted exports cannot leave a partial destination.
- All CLI subcommands have success, failure, output-stream, and exit-code
  integration tests.
- One implementation owns Pkl-to-Nix normalization.
- Modular consumer topologies export without textual source rewriting, or the
  temporary compatibility limitation is explicitly documented and tested.

## Phase 6 — Build a durable verification lattice

### Rust gates

- fmt, clippy, default features, all features, and no default features;
- unit, integration, doctest, and CLI tests;
- real MSRV plus current supported toolchain;
- unused-dependency and advisory checks;
- package verification; and
- archive round-trip/version tests.

### Nix gates

- formatter output and formatter check;
- flake evaluation on every maintained system;
- package, app, and dev-shell output evaluation;
- NixOS and Home Manager module fixtures;
- helper assertions split by domain instead of one monolithic shell block;
- Pkl-to-Nix and modular-export golden checks; and
- app metadata/output-shape checks.

### Cross-language gates

- shared valid-fixture parity;
- shared invalid-fixture diagnostic parity;
- Rust versus Nix accessor result parity;
- deterministic sidecar generation with a clean-tree assertion; and
- compatibility tests for the oldest supported sidecar/schema version.

### CI policy

- Map every local authoritative gate to one CI step or one flake check.
- Avoid misleading names: an MSRV step must run the MSRV compiler.
- Keep quick formatting/evaluation failures early.
- Build expensive checks once and reuse Nix/Cargo artifacts where possible.
- Test aarch64 outputs through evaluation or native/cross builders according
  to what the repository actually supports.

### Exit gate

A fresh checkout can run one documented command and prove formatting,
compilation, tests, parity, packaging, and Nix output integrity without
uncommitted generated changes.

## Phase 7 — Documentation and adoption

### Work

- Expand the README into a short quick start rather than a complete manual.
- Add durable documentation for:
  - architecture and ownership boundaries;
  - canonical schema and field reference;
  - modular consumer topology layout;
  - Rust loading, validation, projections, and archives;
  - NixOS and both Home Manager modes;
  - CLI commands, JSON formats, and exit codes;
  - sidecar generation and compatibility;
  - downstream adapter guidance; and
  - troubleshooting and migration.
- Add runnable examples for minimal, multi-host, IPv6, DNS/service, and
  consumer-defined adapter scenarios.
- Document support policy: operating systems, Rust MSRV, Pkl/pklx versions,
  sidecar versions, and semver expectations.
- Generate a migration guide for every breaking schema/API/Nix option change.
- Keep graph-derived architecture output optional and regenerated separately;
  source documentation remains authoritative.

### Exit gate

- A new consumer can define, validate, export, import, and query a minimal
  topology using only documented commands.
- Every public Rust item is documented or intentionally hidden.
- Every public flake app/module/output has a description and example.

## Phase 8 — Migrate canix without losing ownership boundaries

This is the first explicitly cross-repository phase. Build or update a merged
Fleetix+canix graph before implementation.

### Fleetix responsibilities

- Publish the generic schema, validators, projections, modules, CLI, and
  compatibility shims required by the migration.
- Provide a versioned migration guide and fixture demonstrating the supported
  consumer topology pattern.

### Canix responsibilities

- Retain canonical machine, link, domain, service, and user data under canix's
  modular topology sources.
- Move Fleetix-leaked canix projections and aliases back behind
  `config.canix.lib.*`, toolbelt facades, or another correct consumer owner.
- Regenerate checked-in sidecars only through the documented Fleetix producer.
- Update the Fleetix flake input and lock only after the upstream branch is
  published and validated.
- Preserve existing `config.canix.lib.*`, `config.canix-toolbelt.*`, and
  compatibility surfaces during staged migration.

### Migration sequence

1. Add new Fleetix outputs while retaining compatibility shims.
2. Add canix parity assertions comparing old and new projections.
3. Switch one low-risk consumer domain at a time: links, hosts, domains,
   services, then adapters.
4. Regenerate the canix topology sidecar and require a clean second run.
5. Evaluate all affected host and Home Manager outputs.
6. Run canix's repository preflight and rebuild dry-run gates.
7. Remove compatibility paths only after all callers are migrated and a
   repository-wide reference search is clean.

### Downstream gate

```bash
# In Fleetix
nix flake check --no-update-lock-file --accept-flake-config

# In canix after updating the Fleetix input and regenerating topology
nix run .#fleetix-export
nix flake check
canix repo check --all-hosts
```

For rebuild validation, use the canix rebuild dry-run workflow for the
affected hosts rather than activating systems during migration planning.

## Phase 9 — Release and compatibility declaration

Release preparation and publication use the dedicated Rust crate/repository
release workflow; this improvement plan does not itself authorize publishing.

### Work

- Decide whether the accumulated public changes are a pre-1.0 minor release
  or should begin a 1.0 compatibility promise.
- Finalize changelog, migration guide, license files, package metadata,
  documentation links, and supported-version matrix.
- Verify the exact packaged crate contents and build the package from the
  generated archive.
- Tag and publish only after upstream `pklx` packaging and all downstream
  compatibility gates are green.
- Update canix through its consumed-flake update workflow and verify the
  deployed consumer separately.

### Release gate

- All Phase 0–8 exit criteria are green.
- No undocumented breaking changes remain.
- Package, docs, source tag, and flake revision describe the same version.
- A rollback target and compatibility window are documented.

## Phase 10 — Steady-state maintenance

- Require schema changes to include fixture, Rust, Nix, compatibility, and
  migration evidence in the same review.
- Track validation issue codes as a compatibility surface.
- Keep dependencies updated in narrow batches with MSRV and package gates.
- Run periodic unused-dependency, advisory, public-API, dead-code, and docs
  audits.
- Rebuild Graphify after meaningful architecture changes and use it to find
  emerging hubs, thin communities, and undocumented operational surfaces.
- Retire this plan after its durable rules have moved into stable contributor,
  architecture, release, and migration documentation.

## Recommended pull-request sequence

1. **Baseline contract:** formatter, app metadata, feature-gated binary, real
   MSRV gate, licenses, and explicit publication state.
2. **Fixture foundation:** shared valid/invalid fixtures and current-behavior
   snapshots before semantic refactoring.
3. **Canonical schema:** eliminate schema duplication and introduce versioned
   normalized output.
4. **Validation correctness:** typed issues and complete cross-reference,
   network, DNS, and service checks.
5. **Accessor parity:** fix Rust/Nix divergence and add shared parity tests.
6. **Nix modules:** NixOS source contract, integrated/standalone Home Manager,
   precise option types.
7. **Generic boundary:** move canix-specific projections out of Fleetix.
8. **Rust API layout:** typed errors, module split, compatibility shims, docs.
9. **Exporter and CLI:** native modular evaluation, atomic writes, JSON output,
   exit-code tests.
10. **Verification lattice:** feature/MSRV/package/module/parity checks wired
    consistently into Nix and CI.
11. **Documentation:** quick start, architecture, references, examples, and
    migration guide.
12. **Canix migration:** upstream release candidate, parity transition,
    downstream dry runs, and removal of compatibility paths.
13. **Release:** dedicated release workflow after all prior gates pass.

Each pull request should have one primary semantic purpose. Structural moves,
behavioral fixes, dependency changes, generated artifacts, and downstream
input bumps should remain independently reviewable whenever practical.

## Explicitly deferred unless evidence changes

- A multi-crate Cargo workspace: the current crate is not large enough to
  justify that overhead solely for organization.
- A plugin system or generic provider framework.
- Performance optimization without a measured hot path.
- Secret-management machinery; Fleetix currently owns topology, not secrets.
- Unsafe zero-copy optimizations beyond the existing validated rkyv boundary.
- Fleet-specific deployment orchestration inside Fleetix.

## Definition of done

Fleetix's grand improvement effort is complete when:

- all documented baseline, feature, MSRV, packaging, Nix, and parity gates pass;
- one canonical schema produces or constrains every language representation;
- malformed topology is rejected with consistent, actionable diagnostics;
- Rust and Nix projections agree across the shared fixture matrix;
- NixOS and integrated/standalone Home Manager modes are tested and documented;
- exports are deterministic and atomic;
- generic Fleetix contains no canix-owned policy or data;
- canix consumes the released Fleetix contract with clean evaluation and dry
  rebuild evidence;
- public compatibility and migration policy are explicit; and
- durable architecture, contributor, and release documentation supersedes
  this planning document.
