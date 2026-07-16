{
  config,
  lib,
  osConfig ? null,
  ...
}: let
  inherit (lib) mkOption types;
in {
  options.fleetix = {
    topology = mkOption {
      type = types.attrs;
      default = {};
      description = "Fleet topology (home-manager mirror, read from osConfig).";
    };
  };

  config.fleetix.topology = lib.mkIf (osConfig != null) (osConfig.fleetix.topology or {});
}
