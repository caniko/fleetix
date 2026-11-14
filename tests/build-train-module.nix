{pkgs}: let
  inherit (pkgs) lib;
  evaluate = overrides:
    (import "${pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/build-train.nix
        {
          networking.hostName = "builder";
          users.users.operator.isNormalUser = true;
          fleetix.services.buildTrain = {
            enable = true;
            user = "operator";
            package = pkgs.hello;
            admissionContract = "fixture-resource-admission";
          };
        }
        overrides
      ];
    }).config;
  config = evaluate {};
  service = config.systemd.services.fleetix-build-train;
  contract = builtins.fromJSON (builtins.unsafeDiscardStringContext config.environment.etc."fleetix-train/service.json".source.text);
  connection = builtins.fromJSON config.environment.etc."fleetix-train/connection.json".text;
  rootsType = (import ../modules/build-train.nix {inherit config lib pkgs;}).options.fleetix.services.buildTrain.gcRoots.type;
  invalid = evaluate {fleetix.services.buildTrain.planningTimeoutSeconds = 30;};
  removed = evaluate {fleetix.services.buildTrain.retainedDeployment = null;};
  custom = evaluate {
    fleetix.services.buildTrain = {
      runtimeDirectory = "custom-train";
      stateDirectory = "custom-train";
      gcRoots = "/nix/var/nix/gcroots/per-user/operator/custom-train";
      fenceSeconds = 90;
    };
  };
  customConnection = builtins.fromJSON custom.environment.etc."fleetix-train/connection.json".text;
  customContract = builtins.fromJSON (builtins.unsafeDiscardStringContext custom.environment.etc."fleetix-train/service.json".source.text);
  otherOperator = evaluate {
    users.users.other.isNormalUser = true;
    fleetix.services.buildTrain = {
      user = lib.mkForce "other";
      gcRoots = connection.gc_roots;
    };
  };
  otherConnection = builtins.fromJSON otherOperator.environment.etc."fleetix-train/connection.json".text;
  user = evaluate {fleetix.services.buildTrain.serviceScope = "user";};
  userService = user.systemd.user.services.fleetix-build-train;
  userConnection = builtins.fromJSON user.environment.etc."fleetix-train/connection.json".text;
  withoutNamespaces = evaluate {
    security.allowUserNamespaces = false;
    nix.settings.sandbox = false;
    fleetix.services.buildTrain.serviceScope = "user";
  };
in
  assert service.serviceConfig.User == "operator";
  assert service.serviceConfig.RuntimeDirectoryMode == "0700";
  assert service.serviceConfig.RuntimeDirectoryPreserve == "yes";
  assert service.serviceConfig.StateDirectoryMode == "0700";
  assert service.serviceConfig.UMask == "0077";
  assert service.serviceConfig.TimeoutStopSec == 21630;
  assert service.serviceConfig.MemoryMax == "1G";
  assert service.serviceConfig.KillMode == "control-group";
  assert service.restartIfChanged && service.stopIfChanged;
  assert service.serviceConfig.ExecStart == "${lib.getExe pkgs.hello} build-train serve --config ${config.environment.etc."fleetix-train/service.json".source}";
  assert contract.coordinator.fence_seconds == 3600;
  assert connection.builder == "builder";
  assert connection.socket == "/run/fleetix-train/coordinator.sock";
  assert connection.gc_roots == "/nix/var/nix/gcroots/per-user/operator/fleetix-train";
  assert rootsType.check connection.gc_roots;
  assert rootsType.check "/nix/var/nix/gcroots/custom/.private-train";
  assert lib.all (path: !rootsType.check path) [
    "/nix/var/nix/gcroots"
    "/nix/var/nix/gcroots/"
    "/nix/var/nix/gcroots/../fleetix-roots"
    "/nix/var/nix/gcroots/custom/./fleetix-roots"
    "/nix/var/nix/gcroots/custom/../fleetix-roots"
    "/nix/var/nix/gcroots/custom//fleetix-roots"
    "/nix/var/nix/gcroots/custom/fleetix-roots/"
    "/nix/var/nix/gcroots/custom roots"
    "/nix/var/nix/gcroots/custom\nroots"
  ];
  assert lib.elem "d ${connection.gc_roots} 0700 operator - -" config.systemd.tmpfiles.rules;
  assert builtins.any (entry: !entry.assertion && lib.hasInfix "two native queries" entry.message) invalid.assertions;
  assert builtins.any (entry: !entry.assertion && lib.hasInfix "no staged handover" entry.message) removed.assertions;
  assert customConnection.socket == "/run/custom-train/coordinator.sock";
  assert custom.systemd.services.fleetix-build-train.serviceConfig.RuntimeDirectoryPreserve == "yes";
  assert customConnection.preparation_dir == "/var/lib/custom-train/preparation";
  assert customContract.coordinator.fence_seconds == 90;
  assert customConnection.policy != connection.policy;
  assert otherConnection.policy != connection.policy;
  assert config.fleetix.services.buildTrain.serviceScope == "system";
  assert !(config.systemd.user.services ? fleetix-build-train);
  assert !(user.systemd.services ? fleetix-build-train);
  assert userService.unitConfig.ConditionUser == "operator";
  assert userService.wantedBy == ["default.target"];
  assert userService.after == [];
  assert !(userService.serviceConfig ? User);
  assert !(userService.serviceConfig ? RuntimeDirectory);
  assert !(userService.serviceConfig ? StateDirectory);
  assert userService.serviceConfig.MemoryMax == service.serviceConfig.MemoryMax;
  assert userService.serviceConfig.KillMode == "control-group";
  assert userService.serviceConfig.NoNewPrivileges;
  assert userService.serviceConfig.PrivateUsers && userService.serviceConfig.PrivateTmp;
  assert user.users.users.operator.linger;
  assert userConnection.socket == connection.socket;
  assert userConnection.preparation_dir == connection.preparation_dir;
  assert userConnection.gc_roots == connection.gc_roots;
  assert userConnection.policy != connection.policy;
  assert lib.elem "d /run/fleetix-train 0700 operator - -" user.systemd.tmpfiles.rules;
  assert lib.elem "d /var/lib/fleetix-train 0700 operator - -" user.systemd.tmpfiles.rules;
  assert builtins.any (entry: !entry.assertion && lib.hasInfix "private temporary-directory" entry.message) withoutNamespaces.assertions;
    pkgs.writeText "fleetix-build-train-module" "Private operator service, independent planning limits, custom paths and restart-on-policy-change verified\n"
