# fleetix

<!-- simit:badges:start -->

[![CI](https://img.shields.io/badge/CI-drift-2088ff)](.github/workflows/ci.yaml) [![Nix](https://img.shields.io/badge/Nix-drift-5277c3)](flake.nix) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](https://docs.rs/fleetix) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/fleetix)

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

For shared MCP server declarations and client configuration, see
[MCP adapters](MCP.md): `fleetix.lib.mcp`, `fleetix.homeModules.mcp`, and the
`fleetix mcp` managed-entry reconciler.

`fleetix.lib` exposes pure helper functions for downstream flakes that need to
derive runtime data from a generated topology sidecar:

- `hosts.resolveHostAddress` resolves a host through an explicit ordered
  address policy such as `[ "lan" "direct-link" "wg-home" ]`.
- `hosts.vpnProfile` resolves a host-local VPN profile by host and profile name.
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

## Primary GPU compute

An optional `gpu.render = new S.GpuRender { renderNode = "/dev/dri/by-path/pci-0000:03:00.0-render" }`
declares a default game/3D device. This is independent of the video-decoding
`gpu.media` route and the compute request below. Use a stable PCI alias rather
than a probe-order `renderD128` name. Runtime consumers must verify accessibility,
PCI identity and driver support; topology is desired configuration, not proof
that an application rendered on that device.

`lib/topology/GpuContract.pkl` exports the schema's stable-node pattern, vendor
and backend vocabulary, and shared conformance fixtures. Regenerate its Rust/Nix
consumer with `pkl eval --format json lib/topology/GpuContract.pkl -o
lib/generated/gpu-contract.json`. Fleetix tests check producer drift and compare
official Pkl constraint enforcement with Rust parsing. Official `pkl` must be on
PATH for those tests; the Nix Cargo checks supply it.

`fleetix.lib.gpu.routes` projects a host GPU record to independent rendering,
media and compute defaults; `fleetix.lib.gpu.forHost` also retains inventory and
legacy aliases. Absent routes stay disabled. Consumers translate these roles to
their own framework controls and perform live device validation.

Rust applications can reuse `fleetix::gpu::pci_selector` with
`default-features = false`, which excludes the Pkl evaluator and Tokio. Enable
the `pkl` feature for loading/exporting Pkl and trust-source patching, and
`health` for live probes. The default CLI enables both features.

Hosts may declare `gpu.compute = new S.GpuCompute { backend = "oneapi" }`.
The optional request targets the dGPU when present, otherwise the iGPU.
Validation requires `oneapi` on Intel, `rocm` on AMD, or `cuda` on NVIDIA.
Omitting `compute` preserves existing consumer policy. This is a requested
stack, not proof of runtime device availability; package selection and hardware
verification belong to the consuming application/configuration.

## Pkl Helpers

Rust consumers can add `fleetix = { version = "0.2.0", default-features = false }`
to their Cargo dependencies and
call `fleetix::pkl_to_nix::write_with_cache_sync` or its async counterpart.
The library evaluates Pkl through the published `pklx` crate; a Fleetix Nix
flake input is not needed to run the Rust generator. The Fleetix flake remains
available for NixOS/Home Manager modules and Nix helper functions.

Enable `mcp` for the harness adapter APIs, or `cli` for the standalone executable.
Simit generates verification and crates.io publication workflows:
`simit init ci --platform github --runtime nix --publish-crates`.
Release tags are signed exact versions (for example `0.2.0`); publishing is
owned by `.github/workflows/publish-crate.yaml`.
The CI environment retains the repository's Nix module checks; Rust consumers
still resolve and build the published library entirely through Cargo.
See [product boundaries](ARCHITECTURE.md) for the deployment extraction contract.

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
# Or call the standalone CLI directly:
fleetix pkl-to-nix examples/Topology.pkl /tmp/topology.nix
```

`fleetix::pkl_to_nix::{render, write, write_sync}` provides the same generator
to Rust consumers. Writes preserve unchanged files and reuse a private,
dependency-checked cache under `$XDG_CACHE_HOME/fleetix/pkl-to-nix` (or
`~/.cache/fleetix/pkl-to-nix`). Imported files and the modular topology's
flattened sources invalidate the cache by content. Evaluations that read
environment or remote resources are never cached. Pass `--no-cache` to force
reevaluation, for example in a reproducibility check.

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
your option.
