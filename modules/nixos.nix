{fleetixLib}: {
  config,
  lib,
  ...
}: let
  inherit (lib) mkIf mkOption types;
  topologyType = types.attrs;
  hasSource = config.fleetix.source != null;
  hasValue = config.fleetix.value != null;
in {
  options.fleetix = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Expose a Fleetix topology to NixOS consumers.";
    };
    source = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Path to a generated Fleetix Nix sidecar.";
    };
    value = mkOption {
      type = types.nullOr topologyType;
      default = null;
      description = "Inline topology value, useful for tests and generated modules.";
    };
    topology = mkOption {
      type = topologyType;
      default = {};
      description = "The selected Fleetix topology.";
    };
  };

  config = {
    assertions = [
      {
        assertion = !(hasSource && hasValue);
        message = "fleetix.source and fleetix.value are mutually exclusive";
      }
      {
        assertion = !config.fleetix.enable || hasSource || hasValue;
        message = "fleetix.enable requires fleetix.source or fleetix.value";
      }
      {
        assertion = !hasSource && !hasValue || config.fleetix.enable;
        message = "fleetix.source/value require fleetix.enable = true";
      }
    ];
    fleetix.topology = mkIf config.fleetix.enable (
      if hasSource
      then fleetixLib.fromPkl config.fleetix.source
      else config.fleetix.value
    );
  };
}
