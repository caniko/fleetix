# Fleetix boot operations: one-shot host maintenance that must run while
# nothing else is using the affected state.
#
# Some migrations (renaming a directory tree that live processes hold by
# path, retiring a compatibility symlink, rewriting state a user service
# keeps open) are only safe when no user session or user service runs. Boot
# is that moment: systemd creates /run/nologin early in boot and
# systemd-user-sessions.service removes it once logins (and lingering user
# managers) may start. Each operation runs inside that window, ordered before
# systemd-user-sessions.service, and is skipped on a live `switch`, so a
# newly declared operation stays pending until the next reboot.
#
# An operation runs once per definition: success records a marker keyed by a
# hash of the definition, so changing the commands, user or environment
# schedules it again. A failure is logged and leaves no marker, so the next
# boot retries; it never blocks logins beyond `timeoutSeconds`.
{
  config,
  lib,
  pkgs,
  utils,
  ...
}: let
  inherit (lib) mkOption types;
  stateDirectory = "/var/lib/fleetix/boot-operations";

  operationType = types.submodule ({name, ...}: {
    options = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to run this operation at boot.";
      };
      description = mkOption {
        type = types.str;
        default = "Fleetix boot operation ${name}";
        description = "Unit description.";
      };
      user = mkOption {
        type = types.str;
        default = "root";
        description = "Account the commands run as.";
      };
      commands = mkOption {
        type = types.nonEmptyListOf (types.nonEmptyListOf types.str);
        description = "Argument vectors run in order; the first failing command stops the operation.";
        example = [["/run/current-system/sw/bin/true"]];
      };
      environment = mkOption {
        type = types.attrsOf types.str;
        default = {};
        description = "Environment for the commands.";
      };
      path = mkOption {
        type = types.listOf types.package;
        default = [];
        description = "Packages added to the commands' PATH.";
      };
      once = mkOption {
        type = types.bool;
        default = true;
        description = "Run once per definition. When false, the operation runs on every boot.";
      };
      requiresMountsFor = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Paths whose mounts must be available first.";
      };
      before = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Additional system units that must start after this operation.";
      };
      timeoutSeconds = mkOption {
        type = types.ints.positive;
        default = 1800;
        description = "Upper bound on how long the operation may delay logins.";
      };
    };
  });

  enabled = lib.filterAttrs (_: operation: operation.enable) config.fleetix.bootOperations;

  marker = name: operation: let
    identity = builtins.hashString "sha256" (builtins.toJSON {
      inherit (operation) user commands environment;
      path = map toString operation.path;
    });
  in "${stateDirectory}/${name}.${identity}";

  unit = name: operation: {
    inherit (operation) description environment path;
    wantedBy = ["multi-user.target"];
    before = ["systemd-user-sessions.service"] ++ operation.before;
    after = ["local-fs.target" "systemd-tmpfiles-setup.service"];
    # A live switch must neither rerun nor restart a finished operation.
    restartIfChanged = false;
    unitConfig = {
      # /run/nologin exists only between early boot and
      # systemd-user-sessions.service: the boot window.
      ConditionPathExists = ["/run/nologin"] ++ lib.optional operation.once "!${marker name operation}";
      RequiresMountsFor = operation.requiresMountsFor;
    };
    serviceConfig =
      {
        Type = "oneshot";
        User = operation.user;
        ExecStart = map utils.escapeSystemdExecArgs operation.commands;
        TimeoutStartSec = operation.timeoutSeconds;
      }
      // lib.optionalAttrs operation.once {
        # `+` keeps the marker root-owned whatever account ran the commands.
        ExecStartPost = "+${pkgs.coreutils}/bin/install -D -m 0644 /dev/null ${marker name operation}";
      };
  };
in {
  options.fleetix.bootOperations = mkOption {
    type = types.attrsOf operationType;
    default = {};
    description = "One-shot operations that run during boot, before user logins and user services.";
  };

  config.systemd.services = lib.mapAttrs' (name: operation: lib.nameValuePair "fleetix-boot-op-${name}" (unit name operation)) enabled;
}
