# Fleetix trust observer.
#
# Daemonless trust integration for desktop sessions: a systemd --user .path
# unit watches the SSH known_hosts store and triggers a one-shot scan. New
# host keys not declared in the topology's Trust section are offered via
# `notify-send` actions — Integrate patches Trust.pkl and regenerates the
# sidecar, Ignore silences the proposal.
{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkIf mkOption types;
  cfg = config.fleetix.trustObserver;
  review = cfg.components.opensshKnownHosts.reviewExisting;
  scanArgs = builtins.concatStringsSep " " [
    "trust scan"
    "--topology ${toString cfg.topology}"
    "--known-hosts ${cfg.knownHosts}"
    "--state-dir ${cfg.stateDir}"
    "--review-existing ${if review then "true" else "false"}"
    "--notify"
  ] + lib.optionalString (cfg.sidecar != null) " --sidecar ${toString cfg.sidecar}";
in {
  options.fleetix.trustObserver = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Watch SSH trust stores and prompt to declare new host keys.";
    };
    package = mkOption {
      type = types.package;
      description = "The fleetix binary (built with the cli feature).";
    };
    topology = mkOption {
      type = types.path;
      description = "Path to the modular topology entrypoint (Topology.aggregated.pkl).";
    };
    sidecar = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Generated Nix sidecar regenerated after an interactive integrate.";
    };
    knownHosts = mkOption {
      type = types.str;
      default = "${config.home.homeDirectory}/.ssh/known_hosts";
      description = "Trust store to watch.";
    };
    stateDir = mkOption {
      type = types.str;
      default = "${config.home.homeDirectory}/.local/state/fleetix/trust";
      description = "Observer decisions directory (suppressed proposal ids).";
    };
    components.opensshKnownHosts.reviewExisting = mkOption {
      type = types.bool;
      default = true;
      description = "Offer existing undeclared entries on first enable. Set false to silently baseline them.";
    };
  };

  config = mkIf cfg.enable {
    home.packages = [pkgs.libnotify];

    systemd.user.services."fleetix-trust-scan" = {
      Unit = {
        Description = "Fleetix SSH trust observer";
        After = ["graphical-session.target"];
      };
      Service = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/fleetix ${scanArgs}";
      };
      Install.WantedBy = ["graphical-session.target"];
    };

    systemd.user.paths."fleetix-trust-scan" = {
      Unit = {
        Description = "Watch OpenSSH known_hosts for new trust";
        After = ["graphical-session.target"];
      };
      Path.PathChanged = cfg.knownHosts;
      Install.WantedBy = ["graphical-session.target"];
    };
  };
}
