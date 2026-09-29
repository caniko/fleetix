{
  pkgs,
  homeManager,
}: let
  inherit (pkgs) lib;
  fleetixLib = import ../lib {inherit lib;};
  home = homeManager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      (import ../modules/mcp.nix {inherit fleetixLib;})
      {
        home = {
          username = "tester";
          homeDirectory = "/home/tester";
          stateVersion = "24.11";
        };
        fleetix.mcp = {
          enable = true;
          package = pkgs.hello;
          harnesses = {
            opencode.enable = true;
            omp.enable = true;
            codex.enable = true;
            zed.enable = true;
          };
          servers = {
            drawing = {
              command = "/bin/drawing";
              enabled = false;
              timeoutMs = 210000;
              overrides.zed.timeoutMs = null;
            };
            secret = {
              command = "/bin/secret";
              env.TOKEN.file = "/run/keys/token";
            };
            graph.url = "http://localhost:8781/mcp";
          };
        };
      }
    ];
  };
  cfg = home.config;
in
  assert builtins.all (a: a.assertion) cfg.assertions;
  assert cfg.programs.opencode.settings.mcp.servers.drawing.disabled;
  assert cfg.programs.opencode.settings.mcp.servers.secret.environment.TOKEN == "{file:/run/keys/token}";
  assert cfg.fleetix.mcp.rendered.omp.mcpServers.drawing.timeout == 210000;
  assert (cfg.fleetix.mcp.rendered.omp.mcpServers.secret.env or {}) == {};
  assert lib.hasPrefix "/nix/store/" cfg.fleetix.mcp.rendered.omp.mcpServers.secret.command;
  assert cfg.programs.zed-editor.userSettings.context_servers.graph.url == "http://localhost:8781/mcp";
  assert builtins.length cfg.fleetix.mcp.manifest.targets == 1;
  assert (builtins.head cfg.fleetix.mcp.manifest.targets).name == "codex";
  assert builtins.hasAttr "fleetixMcp" cfg.home.activation;
  assert builtins.hasAttr "/home/tester/.omp/agent/mcp.json" cfg.home.file; true
