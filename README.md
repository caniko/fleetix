# fleetix

Fleet topology library — typed Pkl schema, Rust bindings, and Nix modules.

`fleetix` provides a single source of truth for fleet-wide host, link,
domain, and service topology. A Pkl schema defines the data shape; the
Rust crate consumes Pkl-evaluated output at build time; Nix modules
expose the resulting topology to NixOS and Home Manager configurations.

## Layout

- `src/` — Rust crate (`fleetix`) with topology types, accessors, and a CLI.
- `examples/Topology.pkl` — self-contained Pkl schema + fleet data.
- `lib/` — Nix sidecar expressions generated from Pkl.
- `modules/nixos` and `modules/home-manager` — consumer modules.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
