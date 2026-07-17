{
  config,
  lib,
  osConfig ? null,
  ...
}: let
  inherit (lib) mkIf mkOption types;
  topologyType = types.attrs;
  integrated = osConfig != null;
  hasSource = config.fleetix.source != null;
  hasValue = config.fleetix.value != null;
  mirrored = integrated && (osConfig.fleetix.topology or null);
in {
  options.fleetix = {
    enable = mkOption {
      type = types.bool;
      default = integrated;
      description = "Expose a Fleetix topology to Home Manager consumers.";
    };
    source = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Path to a generated Fleetix Nix sidecar for standalone Home Manager.";
    };
    value = mkOption {
      type = types.nullOr topologyType;
      default = null;
      description = "Inline topology value for standalone Home Manager tests.";
    };
    topology = mkOption {
      type = topologyType;
      default = {};
      readOnly = true;
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
        assertion = integrated || !config.fleetix.enable || hasSource || hasValue;
        message = "standalone fleetix.enable requires fleetix.source or fleetix.value";
      }
      {
        assertion = !hasSource && !hasValue || config.fleetix.enable;
        message = "fleetix.source/value require fleetix.enable = true";
      }
    ];
    fleetix.topology = mkIf config.fleetix.enable (
      if integrated
      then mirrored
      else if hasSource
      then builtins.import config.fleetix.source
      else config.fleetix.value
    );
  };
}
