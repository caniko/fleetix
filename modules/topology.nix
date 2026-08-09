# Fleetix topology module: options and assertions shared by the NixOS and
# Home Manager variants. Integrated (Home Manager with an `osConfig`) mirrors
# the active NixOS module's topology; standalone usage loads a sidecar or an
# inline value, exactly like the NixOS module.
{fleetixLib}: {
  config,
  lib,
  ...
}: let
  inherit (lib) mkIf mkOption types;
  osConfig = config._module.args.osConfig or null;
  integrated = osConfig != null;
  topologyType = types.attrs;
  hasSource = config.fleetix.source != null;
  hasValue = config.fleetix.value != null;
  mirrored =
    if integrated
    then (osConfig.fleetix.topology or null)
    else null;
in {
  options.fleetix = {
    enable = mkOption {
      type = types.bool;
      default = integrated;
      description = "Expose a Fleetix topology to consumers.";
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
        assertion = integrated || !config.fleetix.enable || hasSource || hasValue;
        message = "fleetix.enable requires an integration (osConfig), source, or value";
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
      then fleetixLib.fromPkl config.fleetix.source
      else config.fleetix.value
    );
  };
}
