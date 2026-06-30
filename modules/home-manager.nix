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

  config.fleetix = {};
}
