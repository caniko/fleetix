{ fleetixLib }:
{ config, lib, ... }:
let
  inherit (lib) mkOption types;
in {
  options.fleetix = {
    topology = mkOption {
      type = types.attrs;
      default = {};
      readOnly = true;
      description = "Fleet topology (home-manager mirror, read from osConfig).";
    };
  };

  # Home-manager mirrors the NixOS value via `osConfig.fleetix.topology`
  # when evaluated as a NixOS module; standalone uses `config.fleetix.source`.
  config.fleetix = {};
}
