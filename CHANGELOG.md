# Changelog

## [Unreleased]

### Fixed

- Reject DNS-dependent WireGuard endpoints and non-IPv4 NAT-PMP gateways before
  isolated namespace deployment
- Quote dynamic host keys in Pkl attribute syntax (`atlas` -> `["atlas"]`) in
  topology fixtures and Nix checks

### Added

- Local-access policies: opt-in `deployment.localAccess` schema, Rust bindings,
  validation, Nix helpers, and a NixOS reconciler that routes selected overlay
  traffic over a verified on-link path with SSH host-key-checked target probes
- Host-local generic VPN profiles with tagged WireGuard connection and NAT-PMP
  port-forwarding models, Rust and Nix accessors, and topology validation
- VPN-bound service endpoints in the Pkl schema and Rust topology model
- simit-managed Forgejo CI workflow with fmt, clippy, test, audit, MSRV, docs,
  and package checks on the atlas runner
- Pre-commit hooks for treefmt, cargo fmt, clippy, audit, MSRV, and nix flake
  check
- simit project metadata configuration
