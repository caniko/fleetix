{
  pkgs,
  fleetixPackage,
}: let
  health = pkgs.writeShellScript "surface-fixture-health" ''
    set -eu
    ${pkgs.systemd}/bin/systemctl is-system-running --quiet
    ${pkgs.systemd}/bin/systemctl is-active --quiet postgresql.service fixture-web.service fixture-worker.service
    ${pkgs.postgresql}/bin/pg_isready --quiet -h /run/postgresql
    ${pkgs.curl}/bin/curl --max-time 2 --fail --silent http://127.0.0.1:8080/release-version >/dev/null
    # Host-root 0600 fixture credentials are accessible only through :idmap.
    test "$(${pkgs.coreutils}/bin/cat /run/fleetix-surface/credentials/token)" = "$1-secret"
  '';
  admission = pkgs.writeShellScript "surface-fixture-admission" ''
    set -eu
    # Fixture-only policy: simulate an incompatible production schema.
    test ! -e "/var/lib/nixos-containers/$5/var/lib/no-promote"
  '';
  release = version:
    (import "${toString pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ../modules/service-surface-guest.nix
        {
          system.stateVersion = "25.11";
          fleetix.serviceSurface = {
            services = ["database" "web" "worker"];
            healthCheck = health;
          };
          environment.etc.release-version.text = version;
          environment.systemPackages = [pkgs.curl];
          services.postgresql = {
            enable = true;
            ensureDatabases = ["surface_test"];
          };
          networking.firewall.allowedTCPPorts = [8080];
          networking.nftables.enable = true;
          systemd.services.fixture-web = {
            wantedBy = ["multi-user.target"];
            serviceConfig = {
              ExecStart = "${pkgs.python3}/bin/python -m http.server 8080 --bind 0.0.0.0 --directory /etc";
              DynamicUser = true;
            };
          };
          systemd.services.fixture-worker = {
            wantedBy = ["multi-user.target"];
            serviceConfig.ExecStart = "${pkgs.coreutils}/bin/sleep infinity";
          };
        }
      ];
    }).config.system.build.toplevel;
  releaseA = release "A";
  releaseB = release "B";
in
  pkgs.testers.runNixOSTest {
    name = "fleetix-service-surfaces";
    nodes.machine = {
      imports = [../modules/service-surfaces.nix];
      system.stateVersion = "25.11";
      virtualisation.memorySize = 4096;
      virtualisation.cores = 2;
      # Use a native host store for Nix registration and nspawn idmapped binds.
      virtualisation.useBootLoader = true;
      # Leave room for both persistent PostgreSQL/container roots.
      virtualisation.diskSize = 8192;
      boot.growPartition = true;
      virtualisation.fileSystems."/".autoResize = true;
      environment.systemPackages = [pkgs.curl];
      system.extraDependencies = [releaseA releaseB];
      networking.firewall.allowedTCPPorts = [8088];
      systemd.services.fixture-host-listener = {
        wantedBy = ["multi-user.target"];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python -m http.server 8088 --bind 0.0.0.0 --directory /etc";
      };
      systemd.tmpfiles.rules = [
        "d /run/surface-credentials/dev 0700 root root -"
        "d /run/surface-credentials/prod 0700 root root -"
        "f /run/surface-credentials/dev/token 0600 root root - dev-secret"
        "f /run/surface-credentials/prod/token 0600 root root - prod-secret"
      ];
      fleetix.serviceSurfaces = {
        enable = true;
        package = fleetixPackage;
        services = ["database" "web" "worker"];
        admissionCheck = admission;
        productionEgressInterface = "eth1";
        dev = {
          hostAddress = "10.233.1.1";
          localAddress = "10.233.1.2";
          credentialsDirectory = "/run/surface-credentials/dev";
        };
        prod = {
          hostAddress = "10.233.2.1";
          localAddress = "10.233.2.2";
          credentialsDirectory = "/run/surface-credentials/prod";
          nameservers = ["192.168.1.2"];
        };
      };
    };
    nodes.upstream = {
      virtualisation.memorySize = 512;
      virtualisation.cores = 1;
      networking.firewall.allowedTCPPorts = [53 8080];
      networking.firewall.allowedUDPPorts = [53];
      services.dnsmasq = {
        enable = true;
        settings = {
          address = "/upstream.test/192.168.1.2";
          listen-address = "192.168.1.2";
          bind-interfaces = true;
          no-resolv = true;
        };
      };
      systemd.services.fixture-upstream = {
        wantedBy = ["multi-user.target"];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python -m http.server 8080 --bind 0.0.0.0 --directory ${pkgs.writeTextDir "ok" "upstream"}";
      };
    };
    testScript = ''
      start_all()
      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("register-nix-paths.service")
      assert machine.succeed("stat -c %d /") == machine.succeed("stat -c %d /nix/store"), "Nix store must be on the native root filesystem"
      upstream.wait_for_unit("fixture-upstream.service")
      upstream.wait_for_unit("dnsmasq.service")
      cli = "${fleetixPackage}/bin/fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json"
      dev = "nixos-container run fleetix-dev -- "
      prod = "nixos-container run fleetix-prod -- "
      sql = "runuser -u postgres -- psql -d surface_test -Atc "

      machine.fail("systemctl is-active --quiet container@fleetix-dev.service")
      machine.fail("systemctl is-active --quiet container@fleetix-prod.service")
      machine.succeed(f"{cli} deploy-dev ${releaseA}")
      machine.fail("systemctl is-active --quiet container@fleetix-prod.service")
      machine.succeed(dev + sql + "\"CREATE TABLE marker(value text); INSERT INTO marker VALUES ('dev');\"")
      machine.succeed(f"{cli} promote --expected-dev ${releaseA}")
      assert machine.succeed(dev + "readlink -f /run/current-system").strip() == machine.succeed(prod + "readlink -f /run/current-system").strip() == "${releaseA}"
      machine.succeed(dev + "test ! -e /var/lib/fleetix-surfaces/prod/system")
      machine.succeed(prod + "test ! -e /var/lib/fleetix-surfaces/dev/system")
      machine.fail(dev + "touch /var/lib/fleetix-surfaces/dev/guest-write")
      machine.fail(prod + "touch /var/lib/fleetix-surfaces/prod/guest-write")
      machine.succeed(prod + sql + "\"CREATE TABLE marker(value text); INSERT INTO marker VALUES ('prod');\"")
      assert machine.succeed(dev + sql + "'SELECT value FROM marker'").strip() == "dev"
      assert machine.succeed(prod + sql + "'SELECT value FROM marker'").strip() == "prod"

      machine.succeed(f"{cli} deploy-dev ${releaseB}")
      assert machine.succeed("curl -fsS http://10.233.1.2:8080/release-version").strip() == "B"
      assert machine.succeed("curl -fsS http://10.233.2.2:8080/release-version").strip() == "A"
      machine.fail(f"{cli} promote --expected-dev ${releaseA}")
      machine.fail(dev + "curl --max-time 2 -fsS http://10.233.2.2:8080/release-version")
      machine.fail(prod + "curl --max-time 2 -fsS http://10.233.1.2:8080/release-version")
      assert machine.succeed(prod + "curl --max-time 5 -fsS http://upstream.test:8080/ok").strip() == "upstream"
      machine.fail(dev + "curl --max-time 2 -fsS http://192.168.1.2:8080/ok")
      machine.succeed("curl -fsS http://127.0.0.1:8088/hostname")
      machine.fail(dev + "curl --max-time 2 -fsS http://10.233.1.1:8088/hostname")
      machine.fail(prod + "curl --max-time 2 -fsS http://10.233.2.1:8088/hostname")
      machine.succeed(dev + "test ! -S /nix/var/nix/daemon-socket/socket")
      machine.succeed(prod + "test ! -S /nix/var/nix/daemon-socket/socket")

      machine.succeed("touch /var/lib/nixos-containers/fleetix-prod/var/lib/no-promote")
      machine.fail(f"{cli} promote --expected-dev ${releaseB}")
      assert machine.succeed("curl -fsS http://10.233.2.2:8080/release-version").strip() == "A"
      machine.succeed("rm /var/lib/nixos-containers/fleetix-prod/var/lib/no-promote")
      machine.succeed(f"{cli} recover")
      machine.succeed(f"{cli} promote --expected-dev ${releaseB}")
      assert machine.succeed(prod + "readlink -f /run/current-system").strip() == "${releaseB}"
      assert machine.succeed(dev + "readlink -f /run/current-system").strip() == "${releaseB}"
      assert machine.succeed(prod + sql + "'SELECT value FROM marker'").strip() == "prod"
      machine.succeed(f"{cli} rollback")
      assert machine.succeed(prod + "readlink -f /run/current-system").strip() == "${releaseA}"
      assert machine.succeed(prod + sql + "'SELECT value FROM marker'").strip() == "prod"

      machine.reboot()
      machine.wait_for_unit("fleetix-surfaces-boot.service")
      assert machine.succeed(prod + "readlink -f /run/current-system").strip() == "${releaseA}"
      assert machine.succeed(dev + "readlink -f /run/current-system").strip() == "${releaseB}"
      assert machine.succeed(prod + sql + "'SELECT value FROM marker'").strip() == "prod"
    '';
  }
