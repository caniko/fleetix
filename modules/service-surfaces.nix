# Native container path/profile semantics:
# https://github.com/NixOS/nixpkgs/blob/nixos-unstable/nixos/modules/virtualisation/nixos-containers.nix
# Never use `nixos-container update`: it evaluates/builds instead of adopting an output.
{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkEnableOption mkIf mkOption types;
  cfg = config.fleetix.serviceSurfaces;
  host = config.networking.hostName;
  declared = config.fleetix.topology.hosts.${host}.serviceSurfaces or null;
  surfaces = ["dev" "prod"];
  canonicalPath = value:
    builtins.match "(/[A-Za-z0-9._-]+)+" value
    != null
    && lib.all (part: part != "." && part != "..") (lib.splitString "/" value);
  ipv4 = types.addCheck types.str (value:
    builtins.match "(0|[1-9][0-9]{0,2})(\\.(0|[1-9][0-9]{0,2})){3}" value
    != null
    && lib.all (part: builtins.fromJSON part <= 255) (lib.splitString "." value));
  containerName = types.strMatching "[a-z][a-z0-9-]{0,11}";
  binding = surface:
    types.submodule {
      options = {
        container = mkOption {
          type = containerName;
          default =
            if declared == null
            then "fleetix-${surface}"
            else declared."${surface}Container" or "fleetix-${surface}";
          description = "Persistent container identity; dev and prod never exchange root filesystems.";
        };
        hostAddress = mkOption {
          type = ipv4;
          description = "Explicit IPv4 address on this surface's host-side private veth.";
        };
        localAddress = mkOption {
          type = ipv4;
          description = "Explicit IPv4 guest address, injected at boot outside the release closure.";
        };
        credentialsDirectory = mkOption {
          type = types.str;
          description = "Private, surface-specific directory of runtime secrets. Never a Nix store path.";
        };
        bindings = mkOption {
          type = types.attrsOf types.str;
          default = {};
          description = "Non-secret runtime bindings (domains, callback URLs, sink endpoints). Mounted at /run/fleetix-surface/bindings.json; secrets belong in credentialsDirectory.";
        };
        nameservers = mkOption {
          type = types.listOf ipv4;
          default = [];
          description = "Explicit guest DNS resolvers supplied at runtime. Use 127.0.0.1 for a resolver inside the stack; never inherit the host's loopback stub.";
        };
        memoryMax = mkOption {
          type = types.str;
          default =
            if surface == "dev"
            then "2G"
            else "infinity";
          description = "Hard cgroup memory ceiling for the full service stack.";
        };
        cpuQuota = mkOption {
          type = types.nullOr (types.strMatching "[1-9][0-9]*%");
          default =
            if surface == "dev"
            then "100%"
            else null;
          description = "CPU budget as a positive integer percentage; null omits CPUQuota (unlimited).";
        };
        tasksMax = mkOption {
          type = types.either types.ints.positive (types.enum ["infinity"]);
          default =
            if surface == "dev"
            then 1024
            else "infinity";
          description = "Process/thread ceiling for the complete guest.";
        };
        ioWeight = mkOption {
          type = types.ints.between 1 10000;
          default =
            if surface == "dev"
            then 10
            else 100;
          description = "Relative cgroup IO weight; not a latency guarantee or a hard bandwidth ceiling.";
        };
      };
    };
  policy = {
    version = 1;
    inherit host;
    state_dir = cfg.stateDirectory;
    lock_path = cfg.lockPath;
    admission_check = cfg.admissionCheck;
    containers = lib.genAttrs surfaces (surface: cfg.${surface}.container);
    required_services = lib.sort builtins.lessThan cfg.services;
    tools = {
      systemctl = "${config.systemd.package}/bin/systemctl";
      nixos_container = "${pkgs.nixos-container}/bin/nixos-container";
      nix_store = "${config.nix.package}/bin/nix-store";
    };
  };
  policyPath = config.environment.etc."fleetix-surfaces/policy.json".source;
  overlap = a: b: a == b || lib.hasPrefix "${a}/" b || lib.hasPrefix "${b}/" a;
  interface = surface: "ve-${cfg.${surface}.container}";
  devInterface = interface "dev";
  prodInterface = interface "prod";
in {
  options.fleetix.serviceSurfaces = {
    enable = mkEnableOption "paired, persistent dev/prod service stacks with exact-output promotion" // {default = declared != null;};
    package = mkOption {
      type = types.package;
      description = "Fleetix package built with service-surfaces-cli.";
    };
    services = mkOption {
      type = types.listOf types.str;
      default =
        if declared == null
        then []
        else declared.services or [];
      description = "Complete logical service inventory. Unsupported services block admission rather than being omitted.";
    };
    stateDirectory = mkOption {
      type = types.str;
      default = "/var/lib/fleetix-surfaces";
      description = "Private durable journal, immutable release roots and dev/prod system links.";
    };
    lockPath = mkOption {
      type = types.str;
      default = "/run/lock/fleetix/switch.lock";
      description = "Persistent kernel lock anchor. Every frontend activating this host must use the same path.";
    };
    manageLockAnchor = mkOption {
      type = types.bool;
      default = true;
      description = "Create the persistent activation lock via tmpfiles. Disable when the consumer already owns that exact anchor.";
    };
    enableTun = mkEnableOption "TUN and network-administration capability in both private guest namespaces (required by VPN services)";
    admissionCheck = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Operator-owned immutable executable, independent of the candidate. Receives operation, surface, previous output (or empty), candidate output and container name. Must fail closed on incompatible migrations, stale backups, unsupported services or unsafe rollback. It must be read-only and idempotent.";
    };
    dev = mkOption {type = binding "dev";};
    prod = mkOption {type = binding "prod";};
    productionEgressInterface = mkOption {
      type = types.nullOr (types.strMatching "[A-Za-z0-9_.-]+");
      default = null;
      description = "Explicit production egress/NAT interface. Null denies forwarding; dev forwarding is always denied.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.services != [] && lib.length (lib.unique cfg.services) == lib.length cfg.services;
        message = "fleetix.serviceSurfaces.services must contain the complete, nonempty service inventory without duplicates";
      }
      {
        assertion = cfg.admissionCheck != null && lib.hasPrefix "/nix/store/" (toString cfg.admissionCheck);
        message = "fleetix.serviceSurfaces requires an immutable operator-owned admissionCheck; migrations are not safe by default";
      }
      {
        assertion = canonicalPath cfg.stateDirectory && lib.hasPrefix "/var/lib/" cfg.stateDirectory && canonicalPath cfg.lockPath;
        message = "fleetix.serviceSurfaces requires canonical private state and lock paths";
      }
      {
        assertion = cfg.dev.container != cfg.prod.container;
        message = "fleetix.serviceSurfaces dev/prod container identities must differ";
      }
      {
        assertion =
          lib.all (surface: canonicalPath cfg.${surface}.credentialsDirectory && !lib.hasPrefix "/nix/store/" cfg.${surface}.credentialsDirectory) surfaces
          && !(overlap cfg.dev.credentialsDirectory cfg.prod.credentialsDirectory)
          && lib.all (surface:
            !(overlap cfg.stateDirectory cfg.${surface}.credentialsDirectory)
            && !(overlap "/var/lib/nixos-containers" cfg.${surface}.credentialsDirectory))
          surfaces;
        message = "fleetix.serviceSurfaces credentials directories must be canonical, separate from container roots and the journal, and disjoint between dev/prod";
      }
      {
        assertion = lib.length (lib.unique [cfg.dev.hostAddress cfg.dev.localAddress cfg.prod.hostAddress cfg.prod.localAddress]) == 4;
        message = "fleetix.serviceSurfaces requires four distinct host/guest addresses";
      }
      {
        assertion = lib.versionAtLeast config.system.stateVersion "22.05";
        message = "fleetix.serviceSurfaces requires the modern /var/lib/nixos-containers layout (stateVersion >= 22.05)";
      }
      {
        assertion = cfg.productionEgressInterface == null || cfg.prod.nameservers != [];
        message = "fleetix.serviceSurfaces production egress requires explicit prod.nameservers";
      }
    ];

    boot.enableContainers = true;
    environment.systemPackages = [cfg.package];
    environment.etc."fleetix-surfaces/policy.json".text = builtins.toJSON policy;
    systemd.tmpfiles.rules =
      [
        "d ${cfg.stateDirectory} 0700 root root -"
        "d ${cfg.stateDirectory}/dev 0700 root root -"
        "d ${cfg.stateDirectory}/prod 0700 root root -"
      ]
      ++ lib.optionals cfg.manageLockAnchor [
        "d ${builtins.dirOf cfg.lockPath} 0755 root root -"
        "f ${cfg.lockPath} 0600 root root -"
      ];
    containers = lib.listToAttrs (map (surface: let
        binding = cfg.${surface};
        runtimeBindings = pkgs.writeText "fleetix-${surface}-bindings.json" (builtins.toJSON {
          version = 1;
          inherit host surface;
          inherit (binding) bindings;
        });
        resolvConf =
          pkgs.writeText "fleetix-${surface}-resolv.conf"
          (lib.concatMapStrings (address: "nameserver ${address}\n") binding.nameservers);
      in {
        name = binding.container;
        value = {
          # A string path deliberately keeps the selected guest outside host generations.
          path = lib.mkForce "${cfg.stateDirectory}/${surface}/system";
          # ponytail: the pinned nixpkgs asserts config.nix even for path-only
          # guests. This unused declaration is never built/booted; remove once
          # that assertion is fixed in the producer pin. The forced path wins.
          config = {
            nix.enable = false;
            nixpkgs.pkgs = pkgs;
            system.stateVersion = config.system.stateVersion;
          };
          autoStart = false;
          ephemeral = false;
          privateNetwork = true;
          privateUsers = "pick";
          inherit (cfg) enableTun;
          inherit (binding) hostAddress localAddress;
          bindMounts."/run/fleetix-surface/bindings.json" = {
            hostPath = toString runtimeBindings;
            isReadOnly = true;
          };
          bindMounts."/run/fleetix-surface/resolv.conf" = {
            hostPath = toString resolvConf;
            isReadOnly = true;
          };
          extraFlags =
            [
              "--bind-ro=${binding.credentialsDirectory}:/run/fleetix-surface/credentials:idmap"
            ]
            ++ lib.optional (config.nix.enable && config.nix.daemon.enable)
            "--inaccessible=/nix/var/nix/daemon-socket";
        };
      })
      surfaces);
    systemd.services =
      lib.listToAttrs (map (surface: {
          name = "container@${cfg.${surface}.container}";
          value = {
            restartIfChanged = false;
            stopIfChanged = false;
            unitConfig.RequiresMountsFor = [cfg.${surface}.credentialsDirectory];
            serviceConfig =
              {
                MemoryMax = cfg.${surface}.memoryMax;
                TasksMax = cfg.${surface}.tasksMax;
                IOWeight = cfg.${surface}.ioWeight;
              }
              // lib.optionalAttrs (cfg.${surface}.cpuQuota != null) {
                CPUQuota = cfg.${surface}.cpuQuota;
              };
          };
        })
        surfaces)
      // {
        fleetix-surfaces-boot = {
          description = "Boot only durably verified Fleetix service surfaces";
          wantedBy = ["multi-user.target"];
          wants = ["network-online.target"];
          requires = ["nftables.service"];
          after = ["network-online.target" "nftables.service" "systemd-tmpfiles-setup.service"];
          restartIfChanged = false;
          stopIfChanged = false;
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            UMask = "0077";
            ExecStart = "${cfg.package}/bin/fleetix-surfaces --policy ${policyPath} boot";
          };
        };
      };
    # Explicit filters, not merely absence of NAT: an existing wildcard NAT or
    # routed IPv6 must not let dev reach production or the host control plane.
    networking.nftables = {
      enable = true;
      tables.fleetix-surfaces = {
        family = "inet";
        content = ''
          chain input {
            type filter hook input priority -10; policy accept;
            iifname { "${devInterface}", "${prodInterface}" } ct state established,related accept
            iifname { "${devInterface}", "${prodInterface}" } drop
          }
          chain forward {
            type filter hook forward priority -10; policy accept;
            iifname "${devInterface}" oifname "${prodInterface}" drop
            iifname "${prodInterface}" oifname "${devInterface}" drop
            iifname { "${devInterface}", "${prodInterface}" } ct state established,related accept
            iifname "${devInterface}" drop
            oifname "${devInterface}" drop
            ${lib.optionalString (cfg.productionEgressInterface != null) ''
            iifname "${prodInterface}" oifname "${cfg.productionEgressInterface}" meta nfproto ipv4 accept
          ''}
            iifname "${prodInterface}" drop
          }
        '';
      };
    };
    networking.nat = mkIf (cfg.productionEgressInterface != null) {
      enable = true;
      externalInterface = cfg.productionEgressInterface;
      internalInterfaces = [prodInterface];
    };
  };
}
