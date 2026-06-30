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

## License

Dual-licensed under MIT or Apache-2.0, at your option.
