# fleetix-sidecar

Standalone Rust Pkl-to-Nix generator and embeddable library in the Fleetix Cargo
workspace. Evaluation and rendering use `pklx` in-process. Generation requires
no Nix executable, Pkl subprocess, flake evaluation, or Canix checkout.

```sh
cargo install fleetix-sidecar --path crates/fleetix-sidecar --locked
fleetix-sidecar generate config.pkl generated/config.nix
fleetix-sidecar check config.pkl generated/config.nix
```

`generate` preserves unchanged files and existing permissions, and replaces
changed outputs atomically. Its private cache tracks actual imported/read files;
environment and remote reads bypass caching. Use `--no-cache` for a fresh render.
`--http-rewrite` and `--http-proxy` retain the evaluator's HTTP controls.

`check` always evaluates afresh and compares exact bytes, including the
generated-file header. It writes neither output nor cache. Exit codes: `0` for
success/current, `2` for stale/missing output (and argument errors), `1` for
evaluation or filesystem errors. A consumer may instead compare evaluated Nix
values when it allows formatter-induced byte changes.

Library callers use `render`, `write_with_cache`, and `check` asynchronously,
or `write_with_cache_sync` and `check_sync` outside Tokio. Disable default
features to omit this crate's CLI dependency and executable. Fleetix preserves the same API through
`fleetix::pkl_to_nix` with its `pkl` feature, so existing consumers such as
`canix config generate` share this implementation without spawning a frontend.

Consumer-owned target registries, schemas, topology validation, and output paths
remain in the consumer. This crate converts exactly the supplied Pkl entrypoint.
Publish this prerequisite crate before publishing a Fleetix release that depends
on it; consumers must verify registry availability before changing dependencies.
