{pkgs}: let
  # Each run appends whether logins were still closed, so the log proves both
  # how often an operation ran and that it ran inside the boot window.
  record = log: [
    "${pkgs.bash}/bin/bash"
    "-c"
    "if test -e /run/nologin; then echo boot >> ${log}; else echo live >> ${log}; fi"
  ];
in
  pkgs.testers.runNixOSTest {
    name = "fleetix-boot-operations";
    nodes.machine = {
      imports = [../modules/boot-operations.nix];
      system.stateVersion = "25.11";
      users.users.alice.isNormalUser = true;
      fleetix.bootOperations = {
        once = {
          user = "alice";
          commands = [(record "/home/alice/once.log")];
          requiresMountsFor = ["/home"];
        };
        every = {
          once = false;
          commands = [(record "/var/log/every.log")];
        };
        # Fails until /var/lib/allow-flaky exists: a failure must not block logins
        # and must leave the operation pending for the next boot.
        flaky = {
          commands = [
            (record "/var/log/flaky.log")
            ["${pkgs.coreutils}/bin/test" "-e" "/var/lib/allow-flaky"]
          ];
        };
      };
    };

    testScript = ''
      def lines(path):
          return machine.succeed(f"cat {path}").split()

      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("systemd-user-sessions.service")
      assert lines("/home/alice/once.log") == ["boot"], lines("/home/alice/once.log")
      machine.succeed("stat -c %U /home/alice/once.log | grep -qx alice")
      assert lines("/var/log/every.log") == ["boot"]
      machine.fail("systemctl is-failed --quiet fleetix-boot-op-once.service")
      machine.succeed("systemctl is-failed --quiet fleetix-boot-op-flaky.service")
      machine.succeed("ls /var/lib/fleetix/boot-operations/ | grep -q '^once\\.'")
      machine.fail("ls /var/lib/fleetix/boot-operations/ | grep -q '^flaky\\.'")

      # Outside the boot window a start is skipped by its condition.
      machine.succeed("systemctl start fleetix-boot-op-once.service fleetix-boot-op-every.service")
      assert lines("/home/alice/once.log") == ["boot"]
      assert lines("/var/log/every.log") == ["boot"]

      machine.succeed("touch /var/lib/allow-flaky")
      machine.shutdown()
      machine.start()
      machine.wait_for_unit("multi-user.target")
      machine.wait_for_unit("systemd-user-sessions.service")
      assert lines("/home/alice/once.log") == ["boot"], "once ran again"
      assert lines("/var/log/every.log") == ["boot", "boot"]
      assert lines("/var/log/flaky.log") == ["boot", "boot"], "flaky was not retried"
      machine.succeed("ls /var/lib/fleetix/boot-operations/ | grep -q '^flaky\\.'")
    '';
  }
