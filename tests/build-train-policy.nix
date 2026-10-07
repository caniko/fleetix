# Render the actual service module without instantiating a host or package.
let
  cfg = {
    enable = true;
    builder = "atlas";
    user = "can";
    package.executable = "/tools/fleetix";
    admissionContract = "qualified-resource-policy";
    runtimeDirectory = "fleetix-train";
    stateDirectory = "fleetix-train";
    gcRoots = "/nix/var/nix/gcroots/per-user/can/fleetix-train";
    workers = 2;
    planningWorkers = 1;
    queueLimit = 128;
    agingSeconds = 300;
    planningTimeoutSeconds = 180;
    workerTimeoutSeconds = 21600;
    queryTimeoutSeconds = 60;
    substitutes = true;
    memoryMax = "1G";
    retainedDeployment = null;
  };
  module = import ../modules/build-train.nix {
    config = {
      fleetix.services.buildTrain = cfg;
      nix.package.executable = "/tools/nix";
      networking.hostName = "atlas";
      users.users.can = {};
    };
    lib = {
      getExe = p: p.executable;
      mkIf = condition: content: assert condition; content;
      max = a: b:
        if a > b
        then a
        else b;
      removePrefix = prefix: value: builtins.substring (builtins.stringLength prefix) (-1) value;
    };
    pkgs = {
      coreutils = "/tools/coreutils";
      stdenv.hostPlatform = {
        system = "x86_64-linux";
        isLinux = true;
      };
      writeText = _: text: text;
    };
  };
  command = module.config.systemd.services.fleetix-build-train.serviceConfig.ExecStart;
  prefix = "${cfg.package.executable} build-train serve --config ";
  service = builtins.fromJSON (builtins.substring (builtins.stringLength prefix) (-1) command);
  expected = builtins.fromJSON (builtins.readFile ./fixtures/build-train-operator-service.json);
in
  assert service == expected; service
