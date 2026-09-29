{lib}: let
  mcp = import ../lib/mcp {inherit lib;};
  servers = {
    drawing = {
      command = "/bin/drawing-mcp";
      args = ["--stdio"];
      env.DISPLAY = ":0";
      enabled = false;
      timeoutMs = 210000;
      overrides.kiro.timeoutMs = null;
    };
    graph = {
      url = "http://127.0.0.1:8781/mcp";
      headers.Accept = "application/json";
    };
    private = {
      command = "/bin/private-mcp";
      harnesses = ["codex"];
    };
  };
  render = harness: mcp.render {inherit harness servers;};
  rejects = server:
    !(builtins.tryEval (builtins.deepSeq (mcp.render {
        harness = "omp";
        servers.bad = server;
      })
      true)).success;
  hermesTimeout = ms:
    (mcp.renderServers {
      harness = "hermes";
      servers.tool = {
        command = "/bin/tool";
        timeoutMs = ms;
      };
    }).tool.timeout;
in
  assert builtins.isInt (hermesTimeout 600000);
  assert hermesTimeout 600000 == 600;
  assert hermesTimeout 1250 == 2;
  assert hermesTimeout 1 == 1;
  assert (render "opencode").mcp.servers.drawing.command == ["/bin/drawing-mcp" "--stdio"];
  assert (render "opencode").mcp.servers.drawing.disabled;
  assert (render "opencode").mcp.servers.drawing.timeout.execution == 210000;
  assert !((render "opencode").mcp.servers ? private);
  assert (render "omp").mcpServers.drawing.timeout == 210000;
  assert !(render "omp").mcpServers.drawing.enabled;
  assert (render "omp").mcpServers.drawing.env.DISPLAY == ":0";
  assert (render "omp").mcpServers.graph.headers.Accept == "application/json";
  assert (render "codex").mcp_servers.drawing.tool_timeout_sec == 210;
  assert (render "codex").mcp_servers.graph.http_headers.Accept == "application/json";
  assert (render "codex").mcp_servers.private.command == "/bin/private-mcp";
  assert !((render "claude").mcpServers ? drawing);
  assert (render "gemini").mcpServers.graph.httpUrl == "http://127.0.0.1:8781/mcp";
  assert (render "kiro").mcpServers.drawing.disabled;
  assert (render "copilot").mcpServers.graph.tools == ["*"];
  assert (render "zed").context_servers.graph.url == "http://127.0.0.1:8781/mcp";
  assert (render "hermes").mcp_servers.graph.url == "http://127.0.0.1:8781/mcp";
  assert rejects {
    command = "/bin/x";
    url = "http://example.test/mcp";
  };
  assert rejects {command = "";};
  assert rejects {url = "relative/mcp";};
  assert rejects {
    url = "https://example.test/mcp";
    env.TOKEN = "bad";
  };
  assert rejects {
    command = "/bin/x";
    timeoutMs = -1;
  };
  assert rejects {
    url = "https://example.test/mcp";
    headers.Authorization.bad = "TOKEN";
  };
  assert (mcp.renderServers {
    harness = "codex";
    servers.auth = {
      url = "https://example.test/mcp";
      headers.Authorization.env = "TOKEN";
      startupTimeoutMs = 1250;
    };
  }).auth
  == {
    url = "https://example.test/mcp";
    env_http_headers.Authorization = "TOKEN";
    startup_timeout_sec = 1.25;
  };
  assert (mcp.renderServers {
    harness = "opencode";
    servers.auth = {
      url = "https://example.test/mcp";
      headers.Authorization.env = "TOKEN";
    };
  }).auth.headers.Authorization
  == "{env:TOKEN}";
  assert !(builtins.tryEval (builtins.deepSeq (mcp.render {
      harness = "zed";
      servers.bad = {
        command = "/bin/x";
        timeoutMs = 1000;
      };
    })
    true)).success; true
