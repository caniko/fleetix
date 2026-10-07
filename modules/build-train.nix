{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.fleetix.services.buildTrain;
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
    cfg.admissionContract
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
  };
  serviceConfig = pkgs.writeText "fleetix-train-service.json" (builtins.toJSON {
    inherit native coordinator;
    inherit (cfg) builder;
    admission_contract = cfg.admissionContract;
    memory_max = cfg.memoryMax;
  });
  connection = {
    inherit policy;
    inherit (coordinator) socket;
    inherit (cfg) builder;
    preparation_dir = "${coordinator.state_dir}/preparation";
    inherit (native) gc_roots;
  };
in {
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
      description = "Private directory name under /run. Changing it requires guarded policy rollover.";
    };
    stateDirectory = lib.mkOption {
      type = lib.types.strMatching "[a-zA-Z0-9_-]+";
      default = "fleetix-train";
      description = "Private durable directory name under /var/lib. Preserve it across policy rollover.";
    };
    gcRoots = lib.mkOption {
      type = lib.types.strMatching "/nix/var/nix/gcroots/.+";
      default = "/nix/var/nix/gcroots/per-user/${cfg.user}/fleetix-train";
      description = "Private direct-root namespace; preserve its ownership across policy rollover.";
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
    # Retain this immutable path before activation for guarded offline rollover.
    environment.etc."fleetix-train/service.json".source = serviceConfig;
    systemd.tmpfiles.rules = [
      "d /nix/var/nix/gcroots/per-user/${cfg.user} 0700 ${cfg.user} - -"
      "d ${native.gc_roots} 0700 ${cfg.user} - -"
      "d ${connection.preparation_dir} 0700 ${cfg.user} - -"
    ];
    systemd.services.fleetix-build-train = {
      description = "Fleetix dependency-aware shared construction";
      wantedBy = ["multi-user.target"];
      after = ["nix-daemon.service"];
      # Keep the old immutable execution policy alive until explicitly drained.
      restartIfChanged = false;
      stopIfChanged = false;
      serviceConfig = {
        User = cfg.user;
        ExecStart = "${lib.getExe cfg.package} build-train serve --config ${serviceConfig}";
        RuntimeDirectory = cfg.runtimeDirectory;
        # Offline rollover runs as the operator after the service stops, and
        # must keep the same socket lease inode and operator-owned parent.
        RuntimeDirectoryPreserve = "yes";
        RuntimeDirectoryMode = "0700";
        StateDirectory = cfg.stateDirectory;
        StateDirectoryMode = "0700";
        UMask = "0077";
        Restart = "on-failure";
        RestartSec = 5;
        KillMode = "control-group";
        TimeoutStopSec = lib.max cfg.workerTimeoutSeconds cfg.planningTimeoutSeconds + 30;
        MemoryMax = cfg.memoryMax;
        NoNewPrivileges = true;
        PrivateTmp = true;
      };
    };
  };
}
