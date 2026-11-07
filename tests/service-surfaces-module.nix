{pkgs}: let
  inherit (pkgs) lib;
  evaluate = extra:
    (import "${toString pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/service-surfaces.nix
        {
          networking.hostName = "fixture";
          system.stateVersion = "25.11";
          fleetix.serviceSurfaces = {
            enable = true;
            package = pkgs.hello;
            services = ["database" "web"];
            admissionCheck = "${pkgs.coreutils}/bin/false";
            dev = {
              hostAddress = "10.233.1.1";
              localAddress = "10.233.1.2";
              credentialsDirectory = "/run/keys/dev";
            };
            prod = {
              hostAddress = "10.233.2.1";
              localAddress = "10.233.2.2";
              credentialsDirectory = "/run/keys/prod";
            };
          };
        }
        extra
      ];
    }).config;
  config = evaluate {};
  cfg = config.fleetix.serviceSurfaces;
  policy = builtins.fromJSON (builtins.unsafeDiscardStringContext config.environment.etc."fleetix-surfaces/policy.json".text);
  invalid = evaluate {fleetix.serviceSurfaces.prod.credentialsDirectory = lib.mkForce "/run/keys/dev";};
  missingGuard = evaluate {fleetix.serviceSurfaces.admissionCheck = lib.mkForce null;};
  noDaemon = evaluate {nix.daemon.enable = false;};
  nestedCredentials = evaluate {fleetix.serviceSurfaces.dev.credentialsDirectory = lib.mkForce "/var/lib/nixos-containers/fleetix-prod/credentials";};
  guest =
    (import "${toString pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/service-surface-guest.nix
        {
          system.stateVersion = "25.11";
          fleetix.serviceSurface = {
            services = ["database" "web"];
            healthCheck = "${pkgs.coreutils}/bin/true";
          };
        }
      ];
    }).config;
in
  assert config.containers.fleetix-dev.path == "${cfg.stateDirectory}/dev/system";
  assert config.containers.fleetix-prod.path == "${cfg.stateDirectory}/prod/system";
  assert config.containers.fleetix-dev.privateNetwork && config.containers.fleetix-prod.privateNetwork;
  assert !config.containers.fleetix-dev.autoStart && !config.containers.fleetix-prod.autoStart;
  assert !config.containers.fleetix-dev.ephemeral && !config.containers.fleetix-prod.ephemeral;
  assert config.containers.fleetix-dev.privateUsers == "pick";
  assert config.containers.fleetix-dev.bindMounts."/run/fleetix-surface/bindings.json".isReadOnly;
  assert policy.containers
  == {
    dev = "fleetix-dev";
    prod = "fleetix-prod";
  };
  assert policy.required_services == ["database" "web"];
  assert policy.admission_check == cfg.admissionCheck;
  assert policy.lock_path == cfg.lockPath;
  assert lib.hasInfix "iifname \"ve-fleetix-dev\" drop" config.networking.nftables.tables.fleetix-surfaces.content;
  assert lib.hasInfix "oifname \"ve-fleetix-dev\" drop" config.networking.nftables.tables.fleetix-surfaces.content;
  assert config.systemd.services."container@fleetix-dev".serviceConfig.MemoryMax == "2G";
  assert config.systemd.services."container@fleetix-dev".serviceConfig.TasksMax == 1024;
  assert config.systemd.services."container@fleetix-dev".serviceConfig.IOWeight == 10;
  assert !(config.systemd.services."container@fleetix-prod".serviceConfig ? CPUQuota);
  assert !config.containers.fleetix-dev.enableTun;
  assert lib.elem cfg.dev.credentialsDirectory config.systemd.services."container@fleetix-dev".unitConfig.RequiresMountsFor;
  assert !(lib.elem "--inaccessible=/nix/var/nix/daemon-socket" noDaemon.containers.fleetix-dev.extraFlags);
  assert config.containers.fleetix-dev.bindMounts."/run/fleetix-surface/resolv.conf".isReadOnly;
  assert !config.systemd.services.fleetix-surfaces-boot.restartIfChanged;
  assert !config.systemd.services.fleetix-surfaces-boot.stopIfChanged;
  assert builtins.any (a: !a.assertion && lib.hasInfix "credentials" a.message) invalid.assertions;
  assert builtins.any (a: !a.assertion && lib.hasInfix "admission" a.message) missingGuard.assertions;
  assert builtins.any (a: !a.assertion && lib.hasInfix "credentials" a.message) nestedCredentials.assertions;
  assert guest.boot.isNspawnContainer;
  assert !guest.nix.enable;
  assert guest.networking.hostName == "fleetix";
  assert !guest.networking.resolvconf.enable;
  assert guest.environment.etc."resolv.conf".source == "/run/fleetix-surface/resolv.conf";
  assert guest.fleetix.serviceSurface.manifest.services == ["database" "web"];
  assert lib.hasInfix "fleetix-surface-health" guest.system.systemBuilderCommands;
  assert lib.hasInfix "mkdir -p \"$out/bin\"" guest.system.systemBuilderCommands; true
