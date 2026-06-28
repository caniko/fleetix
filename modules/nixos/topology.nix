{ fleetixLib }:
{ config, lib, ... }:
let
  inherit (lib) mkOption types mkIf;
in {
  options.fleetix = {
    topology = mkOption {
      type = types.attrs;
      default = {};
      readOnly = true;
      description = "Fleet topology loaded from the sidecar Nix expression.";
    };

    source = mkOption {
      type = types.path;
      description = "Path to the fleetix topology sidecar (.nix file).";
    };
  };

  config.fleetix.topology = mkIf (config.fleetix.source != null)
    (fleetixLib.fromPkl config.fleetix.source);
}
