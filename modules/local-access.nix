# Fleetix local-access: prefer a local link for selected overlay traffic.
#
# A policy names client hosts, a target host, a preferred link (usually
# "lan") and a fallback overlay link (usually a WireGuard mesh). On a client,
# the reconciler routes only the selected destination and TCP/UDP ports
# through the local next hop, and only while an on-link path exists and the
# target answers an SSH host-key-checked probe. Everything else keeps using
# the fallback link, whose configuration is never modified.
#
# Return traffic may arrive over the fallback link while the route points at
# the local link, so reverse-path filtering is relaxed to loose on hosts
# with this module enabled. The overlay source address is preserved on the
# policy route, so target-side firewall and service allowlists keep seeing
# the familiar overlay identity.
{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkEnableOption mkIf mkOption types;
  cfg = config.fleetix.localAccess;
  hostname = config.networking.hostName;

  policyType = types.submodule {
    options = {
      clients = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Client hosts this policy applies to.";
      };
      targetHost = mkOption {
        type = types.str;
        description = "Target host reached over the preferred link.";
      };
      preferredLink = mkOption {
        type = types.str;
        default = "lan";
        description = "Link name preferred when it is usable.";
      };
      fallbackLink = mkOption {
        type = types.str;
        default = "wg-home";
        description = "Overlay link whose address stays the packet source.";
      };
      destination = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Destination IP. Defaults to the target's fallback-link address.";
      };
      tcpPorts = mkOption {
        type = types.listOf types.port;
        default = [];
        description = "TCP destination ports routed locally.";
      };
      udpPorts = mkOption {
        type = types.listOf types.port;
        default = [];
        description = "UDP destination ports routed locally.";
      };
      targetLanIp = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Target LAN address used as the on-link next hop.";
      };
      clientSourceIp = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Source address kept on policy-routed packets (client fallback-link address).";
      };
      sshPort = mkOption {
        type = types.port;
        default = 22;
        description = "SSH port for the target identity probe.";
      };
    };
  };

  topologyPolicies = (config.fleetix.topology.deployment or {}).localAccess or {};
  topologyHosts = config.fleetix.topology.hosts or {};
  ownHost = topologyHosts.${hostname} or {};
  linkAddress = host: linkName: (host.links.${linkName}.address or null);

  fromTopology = name: raw: {
    inherit name;
    clients = raw.clients or [];
    targetHost = raw.targetHost;
    preferredLink = raw.preferredLink or "lan";
    fallbackLink = raw.fallbackLink or "wg-home";
    destination =
      if (raw.destination or null) != null
      then raw.destination
      else linkAddress (topologyHosts.${raw.targetHost} or {}) (raw.fallbackLink or "wg-home");
    tcpPorts = raw.tcpPorts or [];
    udpPorts = raw.udpPorts or [];
    targetLanIp = (topologyHosts.${raw.targetHost} or {}).network.lanIp or null;
    clientSourceIp = linkAddress ownHost (raw.fallbackLink or "wg-home");
    sshPort = cfg.sshPort;
  };

  merged =
    builtins.mapAttrs (name: policy: (fromTopology name (topologyPolicies.${name} or {})) // policy)
    (topologyPolicies // cfg.policies);

  active =
    lib.filterAttrs (_: policy: builtins.elem hostname policy.clients)
    merged;

  orderedNames = builtins.attrNames active;
  indexOf = name: let
    go = i: names:
      if names == []
      then 0
      else if builtins.head names == name
      then i
      else go (i + 1) (builtins.tail names);
  in
    go 0 orderedNames;
  tableOf = name: cfg.tableBase + (indexOf name);

  reconcileOne = name: policy: let
    table = tableOf name;
    priority = cfg.rulePriorityBase + (table - cfg.tableBase);
    dest = policy.destination;
    destStr = if dest == null then "" else dest;
    lanIpStr = if policy.targetLanIp == null then "" else policy.targetLanIp;
    srcStr = if policy.clientSourceIp == null then "" else policy.clientSourceIp;
    mkRules = proto: ports:
      lib.concatMapStringsSep "\n" (port: ''
        if ! $IP rule show | $GREP -q "to ${dest}/32 ipproto ${proto} dport ${toString port} .*table ${toString table}"; then
          $IP rule add to ${dest}/32 ipproto ${proto} dport ${toString port} priority ${toString priority} table ${toString table} 2>/dev/null || true
        fi
      '')
      ports;
  in ''
    # Policy ${name}: ${destStr} via ${lanIpStr}
    if [ -n "${destStr}" ] && [ -n "${lanIpStr}" ] && [ -n "${srcStr}" ]; then
      LAN_DEV="$($IP -o route get ${lanIpStr} | $AWK '{for (i=1;i<=NF;i++) if ($i=="dev") print $(i+1)}' | $HEAD -n1)"
      ON_LINK="$($IP -o route get ${lanIpStr} | $GREP -c "scope link" || true)"
      if [ -n "$LAN_DEV" ] && [ "$ON_LINK" -ge 1 ] && [ "$LAN_DEV" != "${cfg.tunnelInterface}" ] && $SSH -o BatchMode=yes -o ConnectTimeout=${toString cfg.sshTimeoutSeconds} -o ConnectionAttempts=1 -p ${toString policy.sshPort} ${cfg.sshUser}@${lanIpStr} true >/dev/null 2>&1; then
        if [ "${destStr}" = "${lanIpStr}" ]; then
          $IP route replace ${destStr}/32 dev "$LAN_DEV" src ${srcStr} table ${toString table} 2>/dev/null || true
        else
          $IP route replace ${destStr}/32 via ${lanIpStr} dev "$LAN_DEV" src ${srcStr} table ${toString table} 2>/dev/null || true
        fi
        ${mkRules "tcp" policy.tcpPorts}
        ${mkRules "udp" policy.udpPorts}
        echo "fleetix-local-access: policy ${name} active via $LAN_DEV"
      else
        while $IP rule del table ${toString table} 2>/dev/null; do :; done
        $IP route flush table ${toString table} 2>/dev/null || true
        echo "fleetix-local-access: policy ${name} inactive, rules withdrawn"
      fi
    fi
  '';

  reconcileScript = pkgs.writeShellScript "fleetix-local-access-reconcile" ''
    set -u
    IP="${pkgs.iproute2}/bin/ip"
    SSH="${pkgs.openssh}/bin/ssh"
    GREP="${pkgs.gnugrep}/bin/grep"
    AWK="${pkgs.gawk}/bin/awk"
    HEAD="${pkgs.coreutils}/bin/head"
    ${lib.concatStringsSep "\n" (lib.mapAttrsToList reconcileOne active)}
  '';
in {
  options.fleetix.localAccess = {
    enable = mkEnableOption "Fleetix local-access LAN preference routing";
    policies = mkOption {
      type = types.attrsOf policyType;
      default = {};
      description = "Extra policies merged over deployment.localAccess from the topology.";
    };
    sshUser = mkOption {
      type = types.str;
      default = "root";
      description = "SSH user for the target identity probe.";
    };
    sshPort = mkOption {
      type = types.port;
      default = 22;
      description = "Default SSH port for target identity probes.";
    };
    sshTimeoutSeconds = mkOption {
      type = types.ints.positive;
      default = 2;
      description = "Per-probe SSH connect timeout in seconds.";
    };
    tunnelInterface = mkOption {
      type = types.str;
      default = "wg-home";
      description = "Interface name that must never count as an on-link LAN device.";
    };
    tableBase = mkOption {
      type = types.int;
      default = 17210;
      description = "First routing-table id used by local-access policies.";
    };
    rulePriorityBase = mkOption {
      type = types.int;
      default = 17210;
      description = "First ip-rule priority used by local-access policies.";
    };
    reconcileInterval = mkOption {
      type = types.str;
      default = "2min";
      description = "systemd OnUnitInactiveSec interval for periodic reconciliation.";
    };
  };

  config = mkIf cfg.enable {
    assertions = lib.mapAttrsToList (name: policy: {
      assertion = policy.destination != null && policy.targetLanIp != null && policy.clientSourceIp != null;
      message = "fleetix.localAccess policy '${name}' needs a destination, target LAN IP, and client source IP from topology";
    }) active;

    # Return traffic may arrive over the fallback tunnel while the policy
    # route points at LAN; strict reverse-path filtering would drop it.
    networking.firewall.checkReversePath = lib.mkDefault "loose";

    systemd.services.fleetix-local-access = {
      description = "Fleetix local-access route reconciler";
      after = ["network-online.target"];
      wants = ["network-online.target"];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = reconcileScript;
      };
    };

    systemd.timers.fleetix-local-access = {
      description = "Periodically reconcile Fleetix local-access routes";
      wantedBy = ["timers.target"];
      timerConfig = {
        OnBootSec = "1min";
        OnUnitInactiveSec = cfg.reconcileInterval;
      };
    };

    networking.networkmanager.dispatcherScripts = [
      {
        type = "basic";
        source = pkgs.writeShellScript "fleetix-local-access-dispatch" ''
          if [ "$2" = "up" ] || [ "$2" = "down" ] || [ "$2" = "connectivity-change" ]; then
            ${pkgs.systemd}/bin/systemctl start fleetix-local-access.service || true
          fi
        '';
      }
    ];
  };
}
