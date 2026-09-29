# Shared MCP adapters

Fleetix owns the server contract, client dialects, and managed-entry writer.
Consumers own server packages, endpoints, credentials, and enablement. The opt-in
module is independent of inference services and OpenPencil.

```nix
{
  imports = [ inputs.fleetix.homeModules.mcp ];
  fleetix.mcp = {
    enable = true;
    harnesses = {
      opencode.enable = true; # native OpenCode V2
      omp.enable = true;      # native ~/.omp/agent/mcp.json
      codex.enable = true;    # merge into writable config.toml
      claude = { enable = true; autoDetect = true; };
    };
    servers = {
      graph.url = "http://127.0.0.1:8781/mcp";
      drawing = {
        command = "${pkgs.drawing-mcp}/bin/drawing-mcp";
        args = [ "--stdio" ];
        enabled = false;
        timeoutMs = 210000;
        overrides.claude.timeoutMs = null;
      };
    };
  };
}
```

## Contract

Exactly one of `command` (stdio) and `url` (Streamable HTTP) is required.
`args`, `env` and `cwd` apply to stdio; `headers` applies to HTTP. `enabled`
controls connection. `harnesses = [ "codex" ]` limits projection to named
clients (`null` selects all). `overrides.<harness>` changes canonical fields
before rendering. Millisecond timeouts become seconds for Codex and Hermes.
Unsupported timeouts or working directories fail evaluation; explicitly override
them to `null` if the client's default is acceptable.

Dialects: OpenCode V2, Codex, Claude Code, OMP, Gemini CLI, Kiro, Copilot CLI,
Zed and Hermes. Clients without a persistent disable field (Claude, Gemini,
Copilot, Hermes) omit disabled servers, preserving the no-connection policy.

Secrets stay outside the store:

```nix
servers.local = {
  command = "/path/to/server";
  env.API_TOKEN.file = "/run/keys/api-token";
};
servers.remote = {
  url = "https://example.org/mcp";
  headers.Authorization.env = "MCP_AUTHORIZATION"; # whole header value
  harnesses = [ "opencode" "codex" "claude" "gemini" ];
  overrides.opencode.extraSettings.oauth = false;
};
```

Home Manager MCP helpers wrap runtime environment files, with native file
interpolation for OpenCode. Environment-backed headers use each supported
client's reference syntax. Other clients reject those references. OAuth and
client-specific settings belong in `overrides.<client>.extraSettings`;
authentication state remains client-owned.

## Delivery and extension

- `native`: one Home Manager owner for OpenCode, OMP or Zed.
- `merge`: owned entries in writable strict JSON or TOML.
- `export`: `fleetix.mcp.rendered.<harness>` for another native owner, including
  Hermes services and clients whose entire config is already managed.

Do not combine merge delivery with a declarative owner. JSONC destinations
require native/export delivery. Pure consumers use `inputs.fleetix.lib.mcp.render
{ harness; servers; }` for documents or `renderServers` for server maps. Pure
rendering accepts literal environment strings; file wrapping needs Home Manager.

A client sharing a dialect needs a harness entry with `dialect`, `root`,
`configPath`, `format` and `delivery`. A new dialect belongs in `lib/mcp/` with
fixtures in `tests/mcp.nix`; server declarations need no changes. Catalogue data
lives in `lib/mcp/Harnesses.pkl`; regenerate with
`fleetix pkl-to-nix lib/mcp/Harnesses.pkl lib/mcp/harnesses.nix`, then treefmt.

## Writable configuration

`fleetix mcp --manifest plan.json --state /absolute/state.json --dry-run`
reports changes without writes; omit `--dry-run` to apply. Version-1 manifests
contain `targets` with `name`, absolute `path`, `format`, nonempty `root` path,
and rendered `servers`. Home Manager exposes `fleetix.mcp.manifest` and runs
the writer after linking the generation.

The ledger contains ownership hashes, never credential values. Identical
pre-existing entries can be adopted. Different existing entries or edits to
owned entries cause conflicts. Resolve the specific entry rather than deleting
the ledger. `adopt` explicitly transfers ownership of selected keys during
migration; `retire` removes a legacy key only for a matching executable suffix
and argument prefix.

All destinations are parsed and conflicts checked before writes. A persistent
kernel lock serializes the writer; atomic replacements preserve permissions.
A journal accepts pre/post-write values after interruption. Unrelated JSON
values and TOML settings/comments survive; unchanged files retain their bytes.
Symlinks are refused. Observed concurrent client edits abort; clients do not share
this lock, so this is not a multi-file transaction with arbitrary client writes.

Disable a harness or remove servers while leaving `fleetix.mcp.enable = true`
for one activation to prune owned entries. Removing the module cannot run
cleanup. Removing auto-detected executables also prunes their entries.

## Validation

Run `cargo test --all-features`, `cargo clippy --all-features --all-targets --
-D warnings`, and treefmt. `tests/mcp.nix` covers dialects and unsupported
capabilities; `tests/mcp-home.nix` evaluates against pinned Home Manager/nixpkgs.
`checks.<system>.mcp-adapters` exposes the pure evaluation gate.

References: [OpenCode V2](https://opencode.ai/v2/docs/mcp-servers), the pinned
Home Manager `modules/lib/mcp.nix`, and OMP's
`packages/coding-agent/src/mcp/types.ts` (milliseconds).
