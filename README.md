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
- `modules/nixos` and `modules/home-manager` — consumer modules.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
