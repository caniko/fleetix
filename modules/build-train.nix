{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.fleetix.services.buildTrain;
  admissionContract = builtins.toJSON [
    "fleetix-train-operator"
    cfg.user
    (
      if cfg.serviceScope == "system"
      then cfg.admissionContract
      else builtins.toJSON ["fleetix-train-user-manager" cfg.admissionContract]
    )
  ];
  native = {
    nix = lib.getExe config.nix.package;
    timeout = "${pkgs.coreutils}/bin/timeout";
    timeout_seconds = cfg.workerTimeoutSeconds;
    query_timeout_seconds = cfg.queryTimeoutSeconds;
    system = pkgs.stdenv.hostPlatform.system;
    gc_roots = cfg.gcRoots;
    inherit (cfg) substitutes;
  };
  # Keep byte parity with native::policy_identity, including preserve_order.
  identity = [
    "fleetix-train-policy"
    2
    cfg.builder
    admissionContract
    [native.nix native.timeout native.timeout_seconds native.query_timeout_seconds native.system native.gc_roots native.substitutes]
    [coordinator.socket coordinator.state_dir coordinator.workers coordinator.planning_workers coordinator.queue_limit coordinator.aging_seconds coordinator.planning_timeout_seconds]
    cfg.memoryMax
  ];
  policy = builtins.hashString "sha256" (builtins.toJSON identity);
  coordinator = {
    inherit policy;
    socket = "/run/${cfg.runtimeDirectory}/coordinator.sock";
    state_dir = "/var/lib/${cfg.stateDirectory}";
    inherit (cfg) workers;
    queue_limit = cfg.queueLimit;
    aging_seconds = cfg.agingSeconds;
    planning_workers = cfg.planningWorkers;
    planning_timeout_seconds = cfg.planningTimeoutSeconds;
    fence_seconds = cfg.fenceSeconds;
  };
  serviceConfig = pkgs.writeText "fleetix-train-service.json" (builtins.toJSON {
    inherit native coordinator;
    inherit (cfg) builder;
    admission_contract = admissionContract;
    memory_max = cfg.memoryMax;
  });
  connection = {
    inherit policy;
    inherit (coordinator) socket;
    inherit (cfg) builder;
    preparation_dir = "${coordinator.state_dir}/preparation";
    inherit (native) gc_roots;
  };
  runtimeDirectoryName = lib.removePrefix "/run/" (builtins.dirOf coordinator.socket);
  stateDirectoryName = lib.removePrefix "/var/lib/" coordinator.state_dir;
  unit = {
    description = "Fleetix dependency-aware shared construction";
    serviceConfig = {
      ExecStart = "${lib.getExe cfg.package} build-train serve --config ${serviceConfig}";
      UMask = "0077";
      Restart = "on-failure";
      RestartSec = 5;
      KillMode = "control-group";
      TimeoutStopSec = lib.max native.timeout_seconds coordinator.planning_timeout_seconds + 30;
      MemoryMax = cfg.memoryMax;
      NoNewPrivileges = true;
      PrivateTmp = true;
    };
  };
in {
  imports = [
    (lib.mkRemovedOptionModule ["fleetix" "services" "buildTrain" "retainedDeployment"] "the coordinator discards a foreign journal at startup; no staged handover is needed")
  ];
  options.fleetix.services.buildTrain = {
    enable = lib.mkEnableOption "builder-local dependency-aware shared construction";
    package = lib.mkOption {
      type = lib.types.package;
      description = "Qualified Fleetix package with cli and build-train-cli features.";
    };
    user = lib.mkOption {
      type = lib.types.str;
      description = "Existing authorized operator account; the private socket accepts this UID only.";
    };
    serviceScope = lib.mkOption {
      type = lib.types.enum ["system" "user"];
      default = "system";
      description = "Manager owning the operator service. User mode enables lingering and sudo-free unit control while retaining existing paths. Changing scope requires qualified host resource/routing policy.";
    };
    builder = lib.mkOption {
      type = lib.types.str;
      default = config.networking.hostName;
      description = "Canonical builder identity supplied by the deployer.";
    };
    admissionContract = lib.mkOption {
      type = lib.types.str;
      description = "Qualified external host-resource admission identity. Live admission remains caller-owned.";
    };
    runtimeDirectory = lib.mkOption {
      type = lib.types.strMatching "[a-zA-Z0-9_-]+";
      default = "fleetix-train";
      description = "Private directory name under /run.";
    };
    stateDirectory = lib.mkOption {
      type = lib.types.strMatching "[a-zA-Z0-9_-]+";
      default = "fleetix-train";
      description = "Private durable directory name under /var/lib. A journal from another policy is discarded at startup.";
    };
    gcRoots = lib.mkOption {
      type = lib.types.addCheck (lib.types.strMatching "/nix/var/nix/gcroots/[A-Za-z0-9_+./-]+") (path:
        lib.all (component: component != "" && component != "." && component != "..")
        (lib.splitString "/" (lib.removePrefix "/nix/var/nix/gcroots/" path)));
      default = "/nix/var/nix/gcroots/per-user/${cfg.user}/fleetix-train";
      description = "Normalized private descendant of /nix/var/nix/gcroots, without empty, dot or parent components.";
    };
    workers = lib.mkOption {
      type = lib.types.ints.between 1 64;
      default = 1;
    };
    queueLimit = lib.mkOption {
      type = lib.types.ints.positive;
      default = 128;
    };
    agingSeconds = lib.mkOption {
      type = lib.types.ints.positive;
      default = 300;
    };
    workerTimeoutSeconds = lib.mkOption {
      type = lib.types.ints.between 1 86400;
      default = 21600;
    };
    queryTimeoutSeconds = lib.mkOption {
      type = lib.types.ints.between 1 300;
      default = 60;
    };
    planningWorkers = lib.mkOption {
      type = lib.types.ints.between 1 16;
      default = 1;
    };
    planningTimeoutSeconds = lib.mkOption {
      type = lib.types.ints.between 1 86400;
      default = 180;
    };
    fenceSeconds = lib.mkOption {
      type = lib.types.ints.between 1 86400;
      default = 3600;
      description = "Activation fence lifetime; an expired fence no longer closes dispatch.";
    };
    substitutes = lib.mkOption {
      type = lib.types.bool;
      default = true;
    };
    memoryMax = lib.mkOption {
      type = lib.types.str;
      default = "1G";
      description = "Coordinator-side memory ceiling; Nix daemon builders retain external host admission.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = pkgs.stdenv.hostPlatform.isLinux;
        message = "buildTrain requires Linux peer authentication.";
      }
      {
        assertion = cfg.serviceScope != "user" || config.security.allowUserNamespaces;
        message = "buildTrain user services require user namespaces for private temporary-directory isolation.";
      }
      {
        assertion = builtins.hasAttr cfg.user config.users.users;
        message = "buildTrain.user must be an existing operator account.";
      }
      {
        assertion = builtins.match "[a-z_][a-z0-9_-]*" cfg.user != null;
        message = "buildTrain.user must be a simple account name.";
      }
      {
        assertion = cfg.builder != "" && cfg.admissionContract != "";
        message = "buildTrain requires nonempty builder and admission identities.";
      }
      {
        assertion = cfg.planningTimeoutSeconds >= 2 * cfg.queryTimeoutSeconds + 20;
        message = "buildTrain planning timeout must cover two native queries and their kill grace periods.";
      }
    ];
    environment.etc."fleetix-train/connection.json".text = builtins.toJSON connection;
    environment.etc."fleetix-train/service.json".source = serviceConfig;
    systemd.tmpfiles.rules =
      [
        "d /nix/var/nix/gcroots/per-user/${cfg.user} 0700 ${cfg.user} - -"
        "d ${connection.gc_roots} 0700 ${cfg.user} - -"
        "d ${connection.preparation_dir} 0700 ${cfg.user} - -"
      ]
      ++ lib.optionals (cfg.serviceScope == "user") [
        # Root provisions these at boot; user service lifecycle must not remove
        # the persistent journal or recreate either coordinator lease anchor.
        "d /run/${runtimeDirectoryName} 0700 ${cfg.user} - -"
        "d /var/lib/${stateDirectoryName} 0700 ${cfg.user} - -"
      ];
    users.users.${cfg.user}.linger = lib.mkIf (cfg.serviceScope == "user") true;
    systemd.services.fleetix-build-train = lib.mkIf (cfg.serviceScope == "system") (unit
      // {
        wantedBy = ["multi-user.target"];
        after = ["nix-daemon.service"];
        serviceConfig =
          unit.serviceConfig
          // {
            User = cfg.user;
            RuntimeDirectory = runtimeDirectoryName;
            # The socket lease inode and operator-owned parent survive restarts.
            RuntimeDirectoryPreserve = "yes";
            RuntimeDirectoryMode = "0700";
            StateDirectory = stateDirectoryName;
            StateDirectoryMode = "0700";
          };
      });
    systemd.user.services.fleetix-build-train = lib.mkIf (cfg.serviceScope == "user") (unit
      // {
        wantedBy = ["default.target"];
        unitConfig.ConditionUser = cfg.user;
        # User units cannot order against the system manager's nix-daemon unit.
        # Native Nix connects to the existing system daemon socket itself.
        serviceConfig = unit.serviceConfig // {PrivateUsers = true;};
      });
  };
}
