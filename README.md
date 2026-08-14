# fleetix

<!-- simit:badges:start -->

[![CI](https://img.shields.io/badge/CI-drift-2088ff)](.forgejo/workflows/ci.yaml) [![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](https://docs.rs/fleetix)

<!-- simit:badges:end -->

Fleet topology library — typed Pkl schema, Rust bindings, and Nix modules.

`fleetix` provides generic tooling for fleet-wide host, link, domain, and
service topology. A consuming flake owns the real topology data and generated
sidecars; Fleetix provides the Pkl schema shape, Rust bindings, export CLI, and
Nix modules that expose a generated topology to NixOS and Home Manager.

## Layout

- `src/` — Rust crate (`fleetix`) with topology types, accessors, and a CLI.
- `examples/Topology.pkl` — self-contained synthetic topology example.
- `lib/topology/` — generic Pkl schema material; no production fleet data.
- `modules/nixos.nix` and `modules/home-manager.nix` — consumer modules.

## Nix Helpers

`fleetix.lib` exposes pure helper functions for downstream flakes that need to
derive runtime data from a generated topology sidecar:

- `hosts.resolveHostAddress` resolves a host through an explicit ordered
  address policy such as `[ "lan" "direct-link" "wg-home" ]`.
- `services.resolveEndpoint` deterministically resolves a local endpoint or
  its declared LAN `remoteVia` endpoint for an ingress host.
- `services.endpointsForHost`, `services.serviceHosts`, and
  `services.managedDnsCnameIntents` derive host, HTTP-site, and DNS views.
- `adapters.infernix.mkFleetNodes` converts fleet host topology plus
  consumer-owned node overlays into Infernix node definitions.

Adapters stay intentionally thin: Fleetix provides network and service
derivation, while consumers such as Infernix keep workload semantics, model
inventory, scheduling policy, and application-specific defaults.

Host `rebuild.buildCache` is backend-neutral fleet policy. It records whether a
host should use a fleet build cache for selected package attributes, while the
consuming flake chooses the concrete cache implementation.

## Pkl Helpers

`fleetix::pkl` exposes generic Rust helpers for downstream crates that keep
their canonical config in Pkl:

- `load(path).await` evaluates a Pkl file and deserializes it into any serde
  model.
- `load_sync(path)` provides the same behavior for synchronous command-line
  code.
- `string_literal(value)` renders strings safely for generated Pkl files.

The generic flake app `pkl-to-nix` evaluates any Pkl file into an importable Nix
sidecar:

```bash
nix run .#pkl-to-nix -- examples/Topology.pkl /tmp/topology.nix
```

Topology-specific consumers should keep using `export-nix` when they need
Fleetix's normalized topology rendering.

## Validation and module integration

Validate a topology before consuming it. Human diagnostics are the default;
automation can request a stable JSON report containing `severity`, `code`,
`path`, `value`, and `message` for every issue:

```bash
fleetix validate examples/Topology.pkl --format json
```

The NixOS module is disabled when `fleetix.source = null` (the default). Set
`fleetix.source` to a generated Nix sidecar to populate `fleetix.topology`:

```nix
{ config, ... }:
{
  imports = [ fleetix.nixosModules.topology ];
  fleetix.source = ./lib/generated/topology.nix;
}
```

The Home Manager module has one integration mode: when imported by Home
Manager with an `osConfig`, it mirrors `osConfig.fleetix.topology`; when used
standalone it remains an empty, opt-in surface. Fleetix does not own a
consumer's machine data or deployment policy.

The declared Rust MSRV is 1.88, required by the resolved `pklr` dependency.
CI also checks the pinned current stable Rust 1.96.1 toolchain so the minimum
compatibility promise and modern compiler behavior are tested separately.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option. The crate is currently marked non-publishable because its git
dependency on `pklx` does not yet have a crates.io release.
