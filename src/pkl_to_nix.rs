//! Compatibility facade for the standalone Fleetix sidecar library.
//!
//! Consumers can keep this API with the `pkl` feature, or depend directly on
//! `fleetix-sidecar` when they need only generic Pkl-to-Nix generation.

pub use fleetix_sidecar::*;
