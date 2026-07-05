# fleetix

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
- `services.serviceEndpoint` derives a reverse-proxy service URL from its
  target host, port, upstream scheme, and address policy.
- `services.reverseProxyServicesForHost` and `firewall.lanExposedPorts` derive
  host-local service/firewall views from reverse-proxy topology.
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

## License

Dual-licensed under MIT or Apache-2.0, at your option.
