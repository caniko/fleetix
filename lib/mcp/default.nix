{lib}: let
  inherit (lib) optionalAttrs;
  catalogue = (import ./harnesses.nix).harnesses;
  nonempty = value: builtins.isString value && value != "";
  positive = value: value == null || (builtins.isInt value && value > 0);
  reference = key: value: builtins.isAttrs value && builtins.attrNames value == [key] && nonempty value.${key};
  defaults = {
    command = null;
    url = null;
    args = [];
    env = {};
    headers = {};
    cwd = null;
    enabled = true;
    timeoutMs = null;
    startupTimeoutMs = null;
    harnesses = null;
    overrides = {};
    extraSettings = {};
  };
  check = name: server:
    assert lib.assertMsg (builtins.isBool server.enabled && builtins.isList server.args && builtins.all builtins.isString server.args) "fleetix.mcp.${name}: enabled must be boolean and args a string list";
    assert lib.assertMsg (builtins.isAttrs server.env && builtins.all (value: builtins.isString value || reference "file" value) (builtins.attrValues server.env)) "fleetix.mcp.${name}: env values must be strings or file references";
    assert lib.assertMsg (builtins.isAttrs server.headers && builtins.all (value: builtins.isString value || reference "env" value) (builtins.attrValues server.headers)) "fleetix.mcp.${name}: headers must be strings or environment references";
    assert lib.assertMsg ((server.command != null) != (server.url != null)) "fleetix.mcp.${name}: exactly one of command or url is required";
    assert lib.assertMsg (server.command == null || nonempty server.command) "fleetix.mcp.${name}: command must not be empty";
    assert lib.assertMsg (server.url == null || builtins.match "https?://[^/[:space:]]+.*" server.url != null) "fleetix.mcp.${name}: URL must be absolute HTTP(S)";
    assert lib.assertMsg (server.url == null || (server.args == [] && server.env == {} && server.cwd == null)) "fleetix.mcp.${name}: args, env and cwd require stdio";
    assert lib.assertMsg (server.headers == {} || server.url != null) "fleetix.mcp.${name}: headers require HTTP";
    assert lib.assertMsg (positive server.timeoutMs && positive server.startupTimeoutMs) "fleetix.mcp.${name}: timeouts must be positive integer milliseconds"; server;
  renderServer = dialect: name: raw: let
    s = check name (defaults // raw);
    local = s.command != null;
    supports = feature: dialects: value:
      assert lib.assertMsg (value == null || builtins.elem dialect dialects) "fleetix.mcp.${name}: ${dialect} cannot represent ${feature}; set an explicit per-harness override"; value;
    timeout = supports "timeoutMs" ["opencode" "omp" "codex" "gemini" "hermes"] s.timeoutMs;
    startup = supports "startupTimeoutMs" ["opencode" "codex"] s.startupTimeoutMs;
    cwd = supports "cwd" ["opencode" "omp" "codex" "hermes"] s.cwd;
    literalHeaders = lib.filterAttrs (_: builtins.isString) s.headers;
    envHeaders = lib.filterAttrs (_: value: builtins.isAttrs value && value ? env) s.headers;
    headerValues = lib.mapAttrs (_: value:
      if builtins.isString value
      then value
      else if dialect == "opencode"
      then "{env:${value.env}}"
      else if builtins.elem dialect ["claude" "gemini"]
      then "\${${value.env}}"
      else throw "fleetix.mcp.${name}: ${dialect} cannot represent environment-backed HTTP headers")
    s.headers;
    common =
      (
        if local
        then {inherit (s) command args;}
        else {inherit (s) url;}
      )
      // optionalAttrs (s.env != {}) {inherit (s) env;}
      // optionalAttrs (cwd != null) {inherit cwd;};
    http = optionalAttrs (s.headers != {}) {headers = headerValues;};
    ms = optionalAttrs (timeout != null) {inherit timeout;};
    seconds = value: value / 1000.0;
    # Hermes' NixOS option accepts integer seconds. Round up so conversion never
    # shortens a positive millisecond timeout; subtract first to avoid overflow.
    wholeSeconds = value: 1 + builtins.div (value - 1) 1000;
    result =
      if dialect == "opencode"
      then
        {
          type =
            if local
            then "local"
            else "remote";
          disabled = !s.enabled;
        }
        // (
          if local
          then {command = [s.command] ++ s.args;}
          else {inherit (s) url;}
        )
        // optionalAttrs (s.env != {}) {environment = s.env;}
        // optionalAttrs (cwd != null) {inherit cwd;}
        // http
        // optionalAttrs (timeout != null || startup != null) {
          timeout = optionalAttrs (timeout != null) {execution = timeout;} // optionalAttrs (startup != null) {inherit startup;};
        }
      else if dialect == "codex"
      then
        common
        // optionalAttrs (!s.enabled) {enabled = false;}
        // optionalAttrs (literalHeaders != {}) {http_headers = literalHeaders;}
        // optionalAttrs (envHeaders != {}) {env_http_headers = lib.mapAttrs (_: value: value.env) envHeaders;}
        // optionalAttrs (timeout != null) {tool_timeout_sec = seconds timeout;}
        // optionalAttrs (startup != null) {startup_timeout_sec = seconds startup;}
      else if dialect == "omp"
      then
        common
        // http
        // ms
        // {
          type =
            if local
            then "stdio"
            else "http";
          inherit (s) enabled;
        }
      else if dialect == "gemini"
      then
        (
          if local
          then common
          else {httpUrl = s.url;}
        )
        // http // ms
      else if dialect == "kiro"
      then common // http // {disabled = !s.enabled;}
      else if dialect == "copilot"
      then
        common
        // http
        // {
          type =
            if local
            then "local"
            else "http";
          tools = ["*"];
        }
      else if dialect == "zed"
      then common // http // {inherit (s) enabled;}
      else if dialect == "hermes"
      then common // http // optionalAttrs (timeout != null) {timeout = wholeSeconds timeout;}
      else if dialect == "claude"
      then
        common
        // http
        // {
          type =
            if local
            then "stdio"
            else "http";
        }
      else throw "fleetix.mcp: unknown dialect '${dialect}'";
  in
    assert lib.assertMsg (builtins.all builtins.isString (builtins.attrValues s.env)) "fleetix.mcp.${name}: environment file references require the Home Manager module";
      builtins.deepSeq [timeout startup cwd] (result // s.extraSettings);
in rec {
  inherit catalogue;
  normalize = name: server: check name (defaults // server);
  select = {
    harness,
    servers,
  }:
    lib.mapAttrs (name: server:
      normalize name (server // ((server.overrides or {}).${harness} or {})))
    (lib.filterAttrs (_: server: (server.harnesses or null) == null || builtins.elem harness server.harnesses) servers);
  renderServers = {
    harness,
    servers,
    dialect ? harness,
  }: let
    selected = select {inherit harness servers;};
    visible = lib.filterAttrs (_: server: server.enabled || !(builtins.elem dialect ["claude" "gemini" "copilot" "hermes"])) selected;
  in
    lib.mapAttrs (renderServer dialect) visible;
  render = {
    harness,
    servers,
    dialect ? harness,
    root ? catalogue.${dialect}.root,
  }:
    lib.setAttrByPath root (renderServers {inherit harness servers dialect;});
}
