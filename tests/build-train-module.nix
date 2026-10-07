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
  connection = builtins.fromJSON config.environment.etc."fleetix-train/connection.json".text;
  invalid = evaluate {fleetix.services.buildTrain.planningTimeoutSeconds = 30;};
  custom = evaluate {
    fleetix.services.buildTrain = {
      runtimeDirectory = "custom-train";
      stateDirectory = "custom-train";
      gcRoots = "/nix/var/nix/gcroots/per-user/operator/custom-train";
    };
  };
  customConnection = builtins.fromJSON custom.environment.etc."fleetix-train/connection.json".text;
in
  assert service.serviceConfig.User == "operator";
  assert service.serviceConfig.RuntimeDirectoryMode == "0700";
  assert service.serviceConfig.RuntimeDirectoryPreserve == "yes";
  assert service.serviceConfig.StateDirectoryMode == "0700";
  assert service.serviceConfig.UMask == "0077";
  assert service.serviceConfig.TimeoutStopSec == 21630;
  assert service.serviceConfig.MemoryMax == "1G";
  assert service.serviceConfig.KillMode == "control-group";
  assert !service.restartIfChanged && !service.stopIfChanged;
  assert service.serviceConfig.ExecStart == "${lib.getExe pkgs.hello} build-train serve --config ${config.environment.etc."fleetix-train/service.json".source}";
  assert connection.builder == "builder";
  assert connection.socket == "/run/fleetix-train/coordinator.sock";
  assert connection.gc_roots == "/nix/var/nix/gcroots/per-user/operator/fleetix-train";
  assert lib.elem "d ${connection.gc_roots} 0700 operator - -" config.systemd.tmpfiles.rules;
  assert builtins.any (entry: !entry.assertion && lib.hasInfix "two native queries" entry.message) invalid.assertions;
  assert customConnection.socket == "/run/custom-train/coordinator.sock";
  assert custom.systemd.services.fleetix-build-train.serviceConfig.RuntimeDirectoryPreserve == "yes";
  assert customConnection.preparation_dir == "/var/lib/custom-train/preparation";
  assert customConnection.policy != connection.policy;
    pkgs.writeText "fleetix-build-train-module" "Private operator service, independent planning limits, custom paths and retained activation policy verified\n"
