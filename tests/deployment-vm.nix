{
  pkgs,
  worker,
}: let
  # Disposable NixOS VM qualification of the real fleetix-deploy worker and
  # real specialisation switch-to-configuration test/boot. The Python
  # admission/health fixtures below are synthetic policy inputs, not
  # production authentication.
  stateRoot = "/var/lib/fleetix-fixture";
  lockPath = "/run/lock/fleetix-fixture.lock";
  requestsRoot = "/var/lib/fleetix-fixture-requests";
  requestPath = "${requestsRoot}/request.json";

  # Synthetic fixture health probe. It is fixed by the immutable policy and
  # deliberately accepts the baseline marker so recovery can verify itself.
  health = pkgs.writeTextFile {
    name = "fleetix-fixture-health";
    executable = true;
    text = ''
      #!${pkgs.python3}/bin/python3
      # NOT production authentication: disposable fixture health only.
      import pathlib
      import sys
      import time

      marker = pathlib.Path("/etc/fleetix-fixture-marker").read_text().strip()
      if marker == "bad":
          time.sleep(4)
      if marker not in ("baseline", "good"):
          print("fixture health: unexpected marker=" + marker, file=sys.stderr)
          sys.exit(1)
    '';
  };

  # Synthetic admission verifier for the stable fixture binding/receipt wire.
  # It is not production authentication; only the test policy invokes it.
  admission = pkgs.writeTextFile {
    name = "fleetix-fixture-admission";
    executable = true;
    text = ''
      #!${pkgs.python3}/bin/python3
      # NOT production authentication: disposable fixture verifier only.
      import hashlib
      import json
      import sys
      import time

      REQUIRED = {
          "version", "target", "attempt", "backend", "candidate",
          "expectedSystem", "expectedProfile", "previousSystem", "previousProfile",
      }
      STORE_KEYS = ("candidate", "expectedSystem", "expectedProfile", "previousSystem", "previousProfile")
      FORGED = "/nix/store/" + "0" * 32 + "-forged-fixture"

      def reject(message):
          print("fixture admission: " + message, file=sys.stderr)
          return 1

      def main():
          raw = sys.stdin.buffer.read()
          binding = json.loads(raw)
          if not isinstance(binding, dict) or set(binding) != REQUIRED:
              return reject("binding fields do not match the fixture contract")
          if binding["version"] != 1 or binding["target"] != "fixture" or binding["backend"] != "nixos":
              return reject("unsupported binding identity")
          for key in STORE_KEYS:
              if not isinstance(binding[key], str) or not binding[key].startswith("/nix/store/"):
                  return reject("invalid " + key)
          attempt = binding["attempt"]
          if not isinstance(attempt, str) or "reject" in attempt or attempt.startswith("deny"):
              return reject("synthetic rejection")
          receipt = {
              "version": 1,
              "target": binding["target"],
              "attempt": attempt,
              "backend": binding["backend"],
              "candidate": binding["candidate"],
              "previousSystem": binding["previousSystem"],
              "previousProfile": binding["previousProfile"],
              "requestDigest": "sha256:" + hashlib.sha256(raw).hexdigest(),
              # Covers 3*90 + 2*5 + 15 = 295 seconds with headroom.
              "expiresAtUnixSeconds": int(time.time()) + 1800,
              "configurationOnly": True,
              "rollbackCompatible": True,
          }
          if "forge" in attempt:
              receipt["configurationOnly"] = False
              receipt["rollbackCompatible"] = False
              receipt["previousProfile"] = FORGED
          print(json.dumps(receipt, separators=(",", ":")))
          return 0

      if __name__ == "__main__":
          sys.exit(main())
    '';
  };

  policy = pkgs.writeText "fleetix-fixture-policy.json" (builtins.toJSON {
    version = 1;
    target = "fixture";
    backend = "nixos";
    inherit stateRoot lockPath;
    profilePath = "/nix/var/nix/profiles/system";
    currentSystemPath = "/run/current-system";
    nixStore = "${pkgs.nix}/bin/nix-store";
    nixEnv = "${pkgs.nix}/bin/nix-env";
    admissionCommand = ["${admission}"];
    healthCommand = ["${health}"];
    commandTimeoutSeconds = 90;
    healthTimeoutSeconds = 5;
    healthIntervalSeconds = 1;
  });
in
  pkgs.testers.runNixOSTest {
    name = "fleetix-recoverable-deployment";
    nodes.machine = {lib, ...}: {
      # The VM does not boot through a boot loader; keep switch-to-configuration
      # itself real while avoiding writes to the disposable root disk.
      boot.loader.grub.enable = false;
      environment.systemPackages = [pkgs.coreutils pkgs.nix];
      environment.etc = {
        "fleetix-fixture.json".source = policy;
        "fleetix-fixture-marker".text = "baseline";
      };

      users.groups.fixture-submitters = {
        members = ["deployer"];
      };
      users.users.deployer.isNormalUser = true;
      users.users.outsider.isNormalUser = true;

      systemd.tmpfiles.rules = [
        "d ${stateRoot} 0700 root root -"
        "f ${lockPath} 0600 root root -"
        "d ${requestsRoot} 2770 root fixture-submitters -"
      ];

      systemd.services.fleetix-fixture-deploy = {
        onFailure = ["fleetix-fixture-recover.service"];
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${worker}/bin/fleetix-deploy run --policy /etc/fleetix-fixture.json --request /root/request.json";
          TimeoutStartSec = 300;
          KillMode = "control-group";
          UMask = "0077";
        };
      };

      # Second fixed root entry point for the no-sudo path. Submitters write
      # only the untrusted request; policy, worker argv and recovery remain
      # administrator-owned. The same recovery unit is its OnFailure target.
      systemd.services.fleetix-fixture-inbox = {
        onFailure = ["fleetix-fixture-recover.service"];
        serviceConfig = {
          Type = "oneshot";
          User = "root";
          Group = "root";
          ExecStart = "${worker}/bin/fleetix-deploy run --policy /etc/fleetix-fixture.json --request ${requestPath}";
          TimeoutStartSec = 300;
          KillMode = "control-group";
          UMask = "0077";
        };
      };

      # Match toolbelt's fixed-file watch and same-filesystem atomic publish.
      # Deliberately not wantedBy boot: later scenarios start it.
      systemd.paths.fleetix-fixture-inbox = {
        wantedBy = [];
        pathConfig = {
          Unit = "fleetix-fixture-inbox.service";
          PathChanged = requestPath;
        };
      };

      systemd.services.fleetix-fixture-lock-holder.serviceConfig = {
        Type = "exec";
        ExecStart = "${pkgs.util-linux}/bin/flock --exclusive ${lockPath} ${pkgs.coreutils}/bin/sleep infinity";
      };

      # Test VMs need not initially have a persisted system profile. Seed it
      # once using Nix itself, without overwriting it on later trial/reboots.
      systemd.services.fleetix-fixture-profile = {
        wantedBy = ["multi-user.target"];
        before = ["fleetix-fixture-recover.service"];
        unitConfig.ConditionPathExists = "!/nix/var/nix/profiles/system";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "${pkgs.nix}/bin/nix-env --profile /nix/var/nix/profiles/system --set /run/current-system";
        };
      };

      # Target-local recovery: runs at boot and when the trial fails. The
      # initiating terminal is never part of the recovery path.
      systemd.services.fleetix-fixture-recover = {
        wantedBy = ["multi-user.target"];
        after = ["fleetix-fixture-profile.service"];
        serviceConfig = {
          Type = "oneshot";
          ExecStartPre = "${pkgs.coreutils}/bin/touch /run/fleetix-fixture-recovery-ran";
          ExecStart = "${worker}/bin/fleetix-deploy recover --policy /etc/fleetix-fixture.json";
          TimeoutStartSec = 300;
          UMask = "0077";
          KillMode = "control-group";
        };
      };

      specialisation.good.configuration = {
        environment.etc."fleetix-fixture-marker".text = lib.mkForce "good";
      };

      specialisation.bad.configuration = {
        environment.etc."fleetix-fixture-marker".text = lib.mkForce "bad";
        # A real root-owned fixture service: its sentinel proves the bad
        # specialisation was actually activated, and survives rollback.
        systemd.services.fleetix-fixture-trial = {
          wantedBy = ["multi-user.target"];
          serviceConfig = {
            Type = "oneshot";
            ExecStart = "${pkgs.coreutils}/bin/touch /run/fleetix-fixture-trial";
          };
        };
      };
    };

    testScript = ''
      import json
      import shlex
      import time

      STATE = "${stateRoot}"
      LOCK = "${lockPath}"
      NIX_STORE = "${pkgs.nix}/bin/nix-store"
      NIX_ENV = "${pkgs.nix}/bin/nix-env"
      ADMISSION = "${admission}"
      HEALTH = "${health}"
      RUNUSER = "${pkgs.util-linux}/bin/runuser"
      PYTHON = "${pkgs.python3}/bin/python3"
      REQUESTS = "${requestsRoot}"
      INBOX = "${requestPath}"
      INBOX_UNIT = "fleetix-fixture-inbox.service"
      INBOX_PATH = "fleetix-fixture-inbox.path"
      COMMITTED = "committed"
      RESTORED = "restored"
      # Journal transaction states are core-owned; the direct journal path is
      # stateRoot/transactions/<attempt>/journal.json.

      start_all()
      machine.wait_for_unit("multi-user.target")
      machine.wait_until_succeeds("test -e /run/fleetix-fixture-recovery-ran")
      machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")
      machine.succeed("! systemctl is-failed --quiet fleetix-fixture-recover.service")

      def run(command):
          return machine.succeed(command).strip()

      def link(path):
          return run("readlink -f " + shlex.quote(path))

      def marker():
          return run("cat /etc/fleetix-fixture-marker")

      def snapshot():
          return (link("/run/current-system"), link("/nix/var/nix/profiles/system"), marker())

      def request(attempt, candidate):
          document = {
              "version": 1,
              "id": attempt,
              "target": "fixture",
              "backend": "nixos",
              "candidate": candidate,
              "expectedSystem": link("/run/current-system"),
              "expectedProfile": link("/nix/var/nix/profiles/system"),
          }
          machine.succeed("printf %s " + shlex.quote(json.dumps(document)) + " > /root/request.json")
          machine.succeed("chmod 0600 /root/request.json")

      def publish(user, attempt, candidate, expected_system, expected_profile, extra=None):
          # The test driver, not the submitter, chooses the fixture values.
          # The write and atomic publish below run with the submitter's euid.
          document = {
              "version": 1,
              "id": attempt,
              "target": "fixture",
              "backend": "nixos",
              "candidate": candidate,
              "expectedSystem": expected_system,
              "expectedProfile": expected_profile,
          }
          if extra:
              document.update(extra)
          code = "\n".join([
              "import json, os, tempfile",
              "document = json.loads(" + repr(json.dumps(document)) + ")",
              "fd, source = tempfile.mkstemp(dir=" + repr(REQUESTS) + ", prefix='.request-')",
              "try:",
              "    with os.fdopen(fd, 'w') as handle:",
              "        json.dump(document, handle, separators=(',', ':'))",
              "    os.replace(source, " + repr(INBOX) + ")",
              "except BaseException:",
              "    os.unlink(source)",
              "    raise",
          ])
          return machine.execute(RUNUSER + " -u " + user + " -- " + PYTHON + " -c " + shlex.quote(code))

      def settle_inbox():
          # A denied request still queues OnFailure recovery; serialize it and
          # reset its start-limit accounting between rapid submissions.
          machine.succeed("systemctl reset-failed fleetix-fixture-recover.service")
          machine.succeed("systemctl start fleetix-fixture-recover.service")
          machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")
          machine.succeed("! systemctl is-failed --quiet fleetix-fixture-recover.service")

      def wait_inbox_watch():
          # The path unit unwatches while the triggered service runs and rearms
          # only after it stops; wait for the watch before publishing again.
          machine.succeed("systemctl start " + INBOX_PATH)
          machine.wait_for_unit(INBOX_PATH)
          machine.wait_until_succeeds(
              "test \"$(systemctl show -p SubState --value " + INBOX_PATH + ")\" = waiting",
              timeout=180,
          )

      def start():
          machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")
          machine.execute("systemctl reset-failed fleetix-fixture-deploy.service")
          return machine.execute("systemctl start fleetix-fixture-deploy.service")

      def settle_recovery():
          # A failed deploy may have queued OnFailure; serialize before the
          # next scenario and leave a fresh recovery-marker timestamp.
          machine.succeed("systemctl start fleetix-fixture-recover.service")
          machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")

      def journal_document(attempt):
          path = STATE + "/transactions/" + attempt + "/journal.json"
          return path, json.loads(run("cat " + shlex.quote(path)))

      def assert_journal(attempt, state, before_system, before_profile, candidate):
          path, document = journal_document(attempt)
          transaction = document["transaction"]
          assert transaction["state"] == state, (path, transaction.get("state"))
          assert transaction["plan"]["id"] == attempt, path
          assert transaction["before"]["system"] == before_system, path
          assert transaction["before"]["profile"] == before_profile, path
          assert transaction.get("trialStarted") is True, path
          assert candidate in json.dumps(document, sort_keys=True), (path, candidate)
          for value in (before_system, before_profile, candidate):
              machine.succeed("test -e " + shlex.quote(value))
              machine.succeed(shlex.quote(NIX_STORE) + " --check-validity " + shlex.quote(value))

      def wait_journal(attempt, state, before_system, before_profile, candidate):
          deadline = time.time() + 60
          while time.time() < deadline:
              try:
                  assert_journal(attempt, state, before_system, before_profile, candidate)
                  return
              except Exception:
                  time.sleep(0.5)
          assert_journal(attempt, state, before_system, before_profile, candidate)

      def assert_rooted(paths):
          for path in paths:
              assert run(shlex.quote(NIX_STORE) + " -q --roots " + shlex.quote(path)), path

      # Immutable policy and private state contract.
      policy = json.loads(run("cat /etc/fleetix-fixture.json"))
      assert policy["version"] == 1 and policy["target"] == "fixture" and policy["backend"] == "nixos"
      assert policy["stateRoot"] == STATE and policy["lockPath"] == LOCK
      assert policy["profilePath"] == "/nix/var/nix/profiles/system" and policy["currentSystemPath"] == "/run/current-system"
      assert policy["nixStore"] == NIX_STORE and policy["nixEnv"] == NIX_ENV
      assert policy["admissionCommand"] == [ADMISSION] and policy["healthCommand"] == [HEALTH]
      assert policy["commandTimeoutSeconds"] == 90 and policy["healthTimeoutSeconds"] == 5 and policy["healthIntervalSeconds"] == 1
      assert run("stat -c %a " + shlex.quote(STATE)) == "700"
      lock_inode = run("stat -c %i " + shlex.quote(LOCK))

      # Genuine contention is the only retryable CLI failure. No request is
      # needed: the worker acquires the shared anchor before reading one.
      baseline = snapshot()
      machine.succeed("systemctl start fleetix-fixture-lock-holder.service")
      machine.wait_until_fails("${pkgs.util-linux}/bin/flock --nonblock " + shlex.quote(LOCK) + " true")
      status, _ = machine.execute("${worker}/bin/fleetix-deploy run --policy /etc/fleetix-fixture.json --request /root/absent.json")
      assert status == 75
      machine.succeed("systemctl stop fleetix-fixture-lock-holder.service")
      assert snapshot() == baseline
      assert run("stat -c %a " + shlex.quote(LOCK)) == "600"
      assert run("systemctl is-enabled fleetix-fixture-recover.service") == "enabled"

      assert marker() == "baseline"
      machine.succeed(HEALTH)
      baseline_system = link("/run/current-system")
      baseline_profile = link("/nix/var/nix/profiles/system")
      good = link("/run/current-system/specialisation/good")
      bad = link("/run/current-system/specialisation/bad")
      assert good != bad
      assert good.startswith("/nix/store/")
      assert bad.startswith("/nix/store/")
      machine.succeed("test ! -e /run/fleetix-fixture-trial")

      # Verified candidate commits both the running system and boot profile.
      request("good-commit", good)
      status, output = start()
      assert status == 0, (status, output)
      assert link("/run/current-system") == good
      assert link("/nix/var/nix/profiles/system") == good
      assert marker() == "good"
      machine.succeed(HEALTH)
      wait_journal("good-commit", COMMITTED, baseline_system, baseline_profile, good)
      assert_rooted([good])
      machine.succeed("test ! -e /run/fleetix-fixture-trial")

      # Keep the good boot profile while making the running system the
      # baseline. Recovery must restore both identities exactly even though
      # they differ.
      machine.succeed(baseline_system + "/bin/switch-to-configuration test")
      machine.wait_until_succeeds("test \"$(readlink -f /run/current-system)\" = " + shlex.quote(baseline_system), timeout=180)
      assert marker() == "baseline"
      machine.succeed(HEALTH)
      machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")

      # Failed health restores the exact previous running system and boot
      # profile, and the journal transaction records the restored state.
      before_bad = snapshot()
      assert before_bad == (baseline_system, good, "baseline")
      request("bad-health", bad)
      status, output = start()
      assert status != 0, (status, output)
      machine.wait_until_succeeds("test \"$(readlink -f /run/current-system)\" = " + shlex.quote(before_bad[0]), timeout=180)
      assert snapshot() == before_bad
      machine.succeed(HEALTH)
      wait_journal("bad-health", RESTORED, before_bad[0], before_bad[1], bad)
      assert_rooted([before_bad[0], before_bad[1]])
      machine.succeed("test -e /run/fleetix-fixture-trial")
      machine.succeed("rm -f /run/fleetix-fixture-trial")
      settle_recovery()
      assert snapshot() == before_bad

      # An admission command failure and a forged/invalid receipt must both
      # reject the request before any activation.
      for attempt in ("verifier-reject", "verifier-forge"):
          before = snapshot()
          request(attempt, bad)
          status, output = start()
          assert status != 0, (attempt, status, output)
          assert snapshot() == before
          machine.succeed("test ! -e /run/fleetix-fixture-trial")
          settle_recovery()
          assert snapshot() == before

      # Kill the target-local worker after the trial candidate is current.
      # OnFailure recovery must restore without the initiating terminal.
      settle_recovery()
      before_kill = snapshot()
      assert before_kill == before
      request("kill-bad", bad)
      machine.execute("systemctl reset-failed fleetix-fixture-deploy.service")
      machine.execute("systemctl start --no-block fleetix-fixture-deploy.service")
      deadline = time.time() + 120
      while time.time() < deadline:
          status, output = machine.execute("readlink -f /run/current-system")
          if status == 0 and output.strip() == bad:
              touched, _ = machine.execute("test -e /run/fleetix-fixture-trial")
              if touched == 0:
                  break
          time.sleep(0.2)
      else:
          raise AssertionError("bad candidate never became current")
      machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")
      assert link("/run/current-system") == bad
      assert run("systemctl show -p ActiveState --value fleetix-fixture-deploy.service") == "activating"
      recovery_stamp = run("stat -c %y /run/fleetix-fixture-recovery-ran")
      machine.succeed("systemctl kill --kill-whom=all --signal=KILL fleetix-fixture-deploy.service")
      machine.wait_until_succeeds("test \"$(readlink -f /run/current-system)\" = " + shlex.quote(before_kill[0]), timeout=180)
      machine.wait_until_succeeds("test \"$(readlink -f /nix/var/nix/profiles/system)\" = " + shlex.quote(before_kill[1]), timeout=180)
      machine.wait_until_succeeds("test \"$(cat /etc/fleetix-fixture-marker)\" = " + shlex.quote(before_kill[2]), timeout=180)
      machine.wait_until_succeeds("systemctl is-failed fleetix-fixture-deploy.service")
      assert snapshot() == before_kill
      machine.succeed(HEALTH)
      wait_journal("kill-bad", RESTORED, before_kill[0], before_kill[1], bad)
      assert_rooted([before_kill[0], before_kill[1]])
      machine.wait_until_succeeds("test \"$(stat -c %y /run/fleetix-fixture-recovery-ran)\" != " + shlex.quote(recovery_stamp), timeout=180)
      machine.succeed("test -e /run/fleetix-fixture-trial")

      # State stays private and the persistent lock anchor is never replaced.
      assert run("stat -c %i " + shlex.quote(LOCK)) == lock_inode
      assert run("stat -c %a " + shlex.quote(STATE)) == "700"

      # Abrupt VM loss must leave a durable pending transaction. This uses the
      # VM's real persistent journal; no controller is alive to trigger recovery.
      settle_recovery()
      before_reboot = snapshot()
      boot_id = run("cat /proc/sys/kernel/random/boot_id")
      machine.succeed("rm -f /run/fleetix-fixture-trial; sync")
      request("reboot-bad", bad)
      machine.execute("systemctl reset-failed fleetix-fixture-deploy.service")
      machine.succeed("systemctl start --no-block fleetix-fixture-deploy.service")
      machine.wait_until_succeeds("test \"$(readlink -f /run/current-system)\" = " + shlex.quote(bad), timeout=180)
      machine.crash()
      machine.start()
      machine.wait_for_unit("multi-user.target")
      assert run("cat /proc/sys/kernel/random/boot_id") != boot_id
      wait_journal("reboot-bad", RESTORED, before_reboot[0], before_reboot[1], bad)
      assert snapshot() == before_reboot
      machine.succeed(HEALTH)

      # No-sudo activation through the fixed root inbox. The test driver uses
      # runuser only to publish an untrusted request; the worker, policy and
      # argv stay root-owned, and no sudo or polkit rule is installed.
      settle_recovery()
      machine.wait_until_succeeds("! systemctl is-active --quiet fleetix-fixture-recover.service")
      assert run("stat -c '%a %U %G' " + REQUESTS) == "2770 root fixture-submitters"
      deployer_groups = run(RUNUSER + " -u deployer -- id -Gn").split()
      outsider_groups = run(RUNUSER + " -u outsider -- id -Gn").split()
      assert "fixture-submitters" in deployer_groups
      assert "fixture-submitters" not in outsider_groups
      assert "wheel" not in deployer_groups
      assert run(RUNUSER + " -u deployer -- id -u") != "0"
      status, _ = machine.execute(RUNUSER + " -u deployer -- sudo -n true")
      assert status != 0, "fixture deployer unexpectedly has passwordless sudo"
      status, _ = machine.execute("systemctl is-active --quiet " + INBOX_PATH)
      assert status != 0, "inbox watcher must not run before the isolated scenarios"
      machine.succeed("systemctl start " + INBOX_PATH)
      machine.wait_for_unit(INBOX_PATH)
      assert run("systemctl show -p User --value " + INBOX_UNIT) in ("", "root")
      assert "User=root" in run("systemctl cat " + INBOX_UNIT)
      inbox_exec = run("systemctl show -p ExecStart --value " + INBOX_UNIT)
      assert "/etc/fleetix-fixture.json" in inbox_exec and INBOX in inbox_exec

      # A submitter publishes a complete request as themselves; the path unit
      # starts the fixed root worker and the verified candidate commits.
      before_user = snapshot()
      assert before_user == before_reboot
      machine.succeed("systemctl reset-failed " + INBOX_UNIT)
      status, output = publish("deployer", "user-good", good, before_user[0], before_user[1])
      assert status == 0, (status, output)
      assert run("stat -c %U " + INBOX) == "deployer"
      wait_journal("user-good", COMMITTED, before_user[0], before_user[1], good)
      machine.wait_until_succeeds("! systemctl is-active --quiet " + INBOX_UNIT, timeout=180)
      machine.succeed("! systemctl is-failed --quiet " + INBOX_UNIT)
      wait_inbox_watch()
      assert run("systemctl show -p Result --value " + INBOX_UNIT) == "success"
      assert snapshot() == (good, good, "good")
      machine.succeed(HEALTH)
      assert_rooted([good])

      # The submitter cannot read the private journal or alter the policy.
      journal = STATE + "/transactions/user-good/journal.json"
      machine.succeed("test -e " + shlex.quote(journal))
      status, _ = machine.execute(RUNUSER + " -u deployer -- " + PYTHON + " -c " + shlex.quote("open(" + repr(journal) + ").read()"))
      assert status != 0, "deployer read the private journal"
      status, _ = machine.execute(RUNUSER + " -u deployer -- " + PYTHON + " -c " + shlex.quote("open('/etc/fleetix-fixture.json', 'a').close()"))
      assert status != 0, "deployer altered the installed policy"

      # A non-member cannot publish and the mutable inbox is unchanged.
      before_outsider = snapshot()
      inbox_digest = run("sha256sum " + INBOX).split()[0]
      inbox_inode = run("stat -c %i " + INBOX)
      machine.succeed("systemctl reset-failed " + INBOX_UNIT)
      status, output = publish("outsider", "outsider-good", good, before_outsider[0], before_outsider[1])
      assert status != 0, (status, output)
      assert run("sha256sum " + INBOX).split()[0] == inbox_digest
      assert run("stat -c %i " + INBOX) == inbox_inode
      assert run("systemctl show -p ActiveState --value " + INBOX_UNIT) == "inactive"
      assert snapshot() == before_outsider
      machine.succeed("test ! -e " + shlex.quote(STATE + "/transactions/outsider-good/journal.json"))

      # The verifier still owns eligibility: the same write path cannot make a
      # denied attempt eligible, and a request cannot smuggle a command.
      before_deny = snapshot()
      assert before_deny == (good, good, "good")
      machine.succeed("systemctl reset-failed " + INBOX_UNIT)
      status, output = publish("deployer", "deny-user", bad, before_deny[0], before_deny[1])
      assert status == 0, (status, output)
      machine.wait_until_succeeds("test \"$(systemctl show -p Result --value " + INBOX_UNIT + ")\" = exit-code", timeout=180)
      machine.wait_until_succeeds("test \"$(systemctl show -p ActiveState --value " + INBOX_UNIT + ")\" = failed", timeout=180)
      wait_inbox_watch()
      assert run("systemctl show -p ExecMainStatus --value " + INBOX_UNIT) == "1"
      assert snapshot() == before_deny
      machine.succeed("test ! -e " + shlex.quote(STATE + "/transactions/deny-user/journal.json"))
      machine.succeed("test ! -e /run/fleetix-fixture-trial")
      settle_inbox()
      assert snapshot() == before_deny

      before_forged = snapshot()
      assert before_forged == before_deny
      machine.succeed("systemctl reset-failed " + INBOX_UNIT)
      status, output = publish(
          "deployer", "unknown-command", bad, before_forged[0], before_forged[1],
          {"command": "touch /run/fleetix-fixture-forged"},
      )
      assert status == 0, (status, output)
      machine.wait_until_succeeds("test \"$(systemctl show -p Result --value " + INBOX_UNIT + ")\" = exit-code", timeout=180)
      machine.wait_until_succeeds("test \"$(systemctl show -p ActiveState --value " + INBOX_UNIT + ")\" = failed", timeout=180)
      wait_inbox_watch()
      assert run("systemctl show -p ExecMainStatus --value " + INBOX_UNIT) == "1"
      assert snapshot() == before_forged
      machine.succeed("test ! -e /run/fleetix-fixture-forged")
      machine.succeed("test ! -e " + shlex.quote(STATE + "/transactions/unknown-command/journal.json"))
      settle_inbox()
      assert snapshot() == before_forged
    '';
  }
