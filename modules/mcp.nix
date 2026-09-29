{fleetixLib}: {
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkOption types mkIf;
  cfg = config.fleetix.mcp;
  inherit (fleetixLib) mcp;
  json = pkgs.formats.json {};
  expand = lib.replaceStrings ["$HOME" "$XDG_CONFIG_HOME"] [config.home.homeDirectory config.xdg.configHome];
  enabled = lib.filterAttrs (_: harness: harness.enable) cfg.harnesses;
  nullableString = default:
    mkOption {
      type = types.nullOr types.str;
      inherit default;
    };
  milliseconds = mkOption {
    type = types.nullOr types.ints.positive;
    default = null;
  };
  serverType = types.submodule {
    options = {
      command = nullableString null;
      url = nullableString null;
      args = mkOption {
        type = types.listOf types.str;
        default = [];
      };
      env = mkOption {
        type = types.attrsOf (types.either types.str (types.submodule {options.file = mkOption {type = types.str;};}));
        default = {};
        description = "Literal environment values or runtime file references; secret contents never enter the Nix store.";
      };
      headers = mkOption {
        type = types.attrsOf (types.either types.str (types.submodule {options.env = mkOption {type = types.str;};}));
        default = {};
        description = "Literal HTTP headers or references to environment variables containing whole header values.";
      };
      cwd = nullableString null;
      enabled = mkOption {
        type = types.bool;
        default = true;
        description = "Connect automatically. Adapters without a disable field omit disabled servers.";
      };
      timeoutMs = milliseconds;
      startupTimeoutMs = milliseconds;
      harnesses = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        description = "Target allowlist; null selects all configured harnesses.";
      };
      overrides = mkOption {
        type = types.attrsOf json.type;
        default = {};
        description = "Explicit canonical field overrides by harness name.";
      };
      extraSettings = mkOption {
        inherit (json) type;
        default = {};
        description = "Client-specific fields; normally set inside a harness override.";
      };
    };
  };
  harnessType = types.submodule ({
    name,
    config,
    ...
  }: let
    known = mcp.catalogue.${config.dialect} or {};
  in {
    options = {
      enable = lib.mkEnableOption "MCP registrations for ${name}";
      dialect = mkOption {
        type = types.enum (builtins.attrNames mcp.catalogue);
        default = name;
        description = "Renderer to use; custom harnesses can reuse an existing dialect.";
      };
      delivery = mkOption {
        type = types.enum ["native" "merge" "export"];
        default =
          if builtins.elem name ["opencode" "omp" "zed"]
          then "native"
          else if name == "hermes"
          then "export"
          else "merge";
        description = "Native Home Manager output, owned-entry reconciliation, or an exported document for a service/other writer.";
      };
      configPath = mkOption {
        type = types.str;
        default = expand (known.path or "");
        defaultText = lib.literalExpression "expanded catalogue path";
      };
      format = mkOption {
        type = types.enum ["json" "toml" "native"];
        default = known.format or "json";
      };
      root = mkOption {
        type = types.listOf types.str;
        default = known.root or [];
      };
      autoDetect = mkOption {
        type = types.bool;
        default = false;
        description = "For merge delivery, configure only if a probe command is on activation PATH.";
      };
      commands = mkOption {
        type = types.listOf types.str;
        default = known.commands or [];
      };
      adopt = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Explicit migration: take ownership of these existing server keys on first reconciliation.";
      };
      retire = mkOption {
        type = types.listOf (types.submodule {
          options = {
            key = mkOption {type = types.str;};
            commandSuffix = mkOption {type = types.str;};
            argsPrefix = mkOption {
              type = types.listOf types.str;
              default = [];
            };
          };
        });
        default = [];
        description = "Remove legacy keys only when their command and argument prefix match a known retired registration.";
      };
    };
  });
  # Reuse Home Manager's runtime-file environment wrapper for clients without
  # native file interpolation. Rendering remains a pure, separately usable API.
  forHarness = name: harness:
    lib.mapAttrs (serverName: selected: let
      inherit (selected) env;
      wrapped =
        lib.hm.mcp.wrapEnvFilesCommand {
          inherit pkgs;
          name = serverName;
        }
        selected;
    in
      (
        if harness.dialect == "opencode"
        then selected // {env = lib.hm.mcp.renderEnv (path: "{file:${path}}") env;}
        else
          wrapped
          // {
            command =
              if wrapped.command == null
              then null
              else toString wrapped.command;
          }
      )
      // {overrides = {};})
    (mcp.select {
      harness = name;
      inherit (cfg) servers;
    });
  rendered = lib.mapAttrs (name: harness:
    mcp.render {
      harness = name;
      inherit (harness) dialect root;
      servers = forHarness name harness;
    })
  enabled;
  mergeHarnesses = lib.filterAttrs (_: harness: harness.delivery == "merge") enabled;
  manifest = {
    version = 1;
    targets =
      lib.mapAttrsToList (name: harness: {
        inherit name;
        path = harness.configPath;
        inherit (harness) format root autoDetect commands adopt retire;
        servers = lib.getAttrFromPath harness.root rendered.${name};
      })
      mergeHarnesses;
  };
  native = name: cfg.enable && (enabled.${name}.delivery or null) == "native";
in {
  options.fleetix.mcp = {
    enable = lib.mkEnableOption "shared MCP harness adapters";
    package = mkOption {
      type = types.nullOr types.package;
      default = null;
      description = "Fleetix with the cli feature, used for writable-file reconciliation.";
    };
    servers = mkOption {
      type = types.attrsOf serverType;
      default = {};
    };
    harnesses = mkOption {
      type = types.attrsOf harnessType;
      default = {};
    };
    stateFile = mkOption {
      type = types.str;
      default = "${config.xdg.stateHome}/fleetix/mcp.json";
      defaultText = lib.literalExpression ''"\${config.xdg.stateHome}/fleetix/mcp.json"'';
    };
    rendered = mkOption {
      type = types.attrsOf json.type;
      readOnly = true;
      description = "Native configuration documents, indexed by enabled harness name.";
    };
    manifest = mkOption {
      inherit (json) type;
      readOnly = true;
      description = "Versioned managed-file reconciliation plan.";
    };
  };
  config = lib.mkMerge [
    {
      fleetix.mcp.rendered = rendered;
      fleetix.mcp.manifest = manifest;
      assertions = lib.optionals cfg.enable (
        [
          {
            assertion = cfg.package != null;
            message = "fleetix.mcp.package must provide the reconciler (including when removing the last target).";
          }
        ]
        ++ lib.mapAttrsToList (name: h: {
          assertion = (h.delivery != "native" || builtins.elem name ["opencode" "omp" "zed"]) && (h.delivery != "merge" || h.format != "native") && (!h.autoDetect || h.delivery == "merge");
          message = "fleetix.mcp.harnesses.${name}: invalid delivery/format/autoDetect combination; use export for a custom native consumer.";
        })
        enabled
        ++ lib.mapAttrsToList (name: server: {
          assertion = server.harnesses == null || builtins.all (h: builtins.hasAttr h cfg.harnesses) server.harnesses;
          message = "fleetix.mcp.servers.${name}: unknown harness in allowlist.";
        })
        cfg.servers
      );
    }
    (mkIf (cfg.enable && cfg.package != null) {
      home.activation.fleetixMcp = lib.hm.dag.entryAfter ["linkGeneration"] ''
        export PATH=${lib.escapeShellArg "${config.home.profileDirectory}/bin"}:$PATH
        run ${lib.getExe cfg.package} mcp --manifest ${json.generate "fleetix-mcp.json" manifest} --state ${lib.escapeShellArg cfg.stateFile}
      '';
    })
    (mkIf (native "opencode") {programs.opencode.settings.mcp = rendered.opencode.mcp;})
    (mkIf (native "zed") {programs.zed-editor.userSettings.context_servers = rendered.zed.context_servers;})
    (mkIf (native "omp") {home.file.${cfg.harnesses.omp.configPath or "${config.home.homeDirectory}/.omp/agent/mcp.json"}.text = builtins.toJSON rendered.omp;})
  ];
}
