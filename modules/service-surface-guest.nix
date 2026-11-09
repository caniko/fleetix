# Build this guest once. Addresses, credentials and environment bindings are
# supplied by the host, not compiled into a different dev/prod system.
{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkOption types;
  cfg = config.fleetix.serviceSurface;
  manifest = pkgs.writeText "fleetix-surface.json" (builtins.toJSON cfg.manifest);
in {
  options.fleetix.serviceSurface = {
    services = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "Complete logical service inventory in this guest release; must match the host policy.";
    };
    healthCheck = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Immutable executable checking the full stack, including databases and workers. Receives surface and expected system output as arguments.";
    };
    manifest = mkOption {
      type = types.attrs;
      readOnly = true;
      default = {
        version = 1;
        services = lib.sort builtins.lessThan cfg.services;
        units = map (name: "${name}.service") (builtins.attrNames (lib.filterAttrs (_: unit: unit.enable) config.systemd.services));
      };
      description = "Release-bound inventory, including every enabled system service, not just web endpoints.";
    };
  };

  config = {
    assertions = [
      {
        assertion = cfg.services != [] && lib.length (lib.unique cfg.services) == lib.length cfg.services;
        message = "fleetix.serviceSurface.services must list every logical service exactly once";
      }
      {
        assertion = cfg.healthCheck != null && lib.hasPrefix "/nix/store/" (toString cfg.healthCheck);
        message = "fleetix.serviceSurface requires an immutable full-stack healthCheck";
      }
    ];
    boot.isNspawnContainer = true;
    networking.hostName = lib.mkForce "fleetix";
    networking.useDHCP = false;
    networking.useHostResolvConf = false;
    networking.resolvconf.enable = false;
    environment.etc."resolv.conf".source = "/run/fleetix-surface/resolv.conf";
    # No host daemon socket or source compilation is needed in a service guest.
    nix.enable = false;
    environment.systemPackages = [pkgs.coreutils];
    system.systemBuilderCommands = lib.mkAfter ''
      mkdir -p "$out/bin"
      cp ${manifest} "$out/fleetix-surface.json"
      ${lib.optionalString (cfg.healthCheck != null) ''
        ln -s ${lib.escapeShellArg (toString cfg.healthCheck)} "$out/bin/fleetix-surface-health"
      ''}
    '';
  };
}
