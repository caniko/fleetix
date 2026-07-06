# Changelog

## [Unreleased]

### Fixed

- Quote dynamic host keys in Pkl attribute syntax (`atlas` -> `["atlas"]`) in
  topology fixtures and Nix checks

### Added

- simit-managed Forgejo CI workflow with fmt, clippy, test, audit, MSRV, docs,
  and package checks on the atlas runner
- Pre-commit hooks for treefmt, cargo fmt, clippy, audit, MSRV, and nix flake
  check
- simit project metadata configuration
