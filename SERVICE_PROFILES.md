# Service profiles and Gatus

`Services.catalog` describes logical services independently of a dashboard.
The catalog key and each key under `health` are stable service/check identifiers.
Profiles own references to existing `endpoints` and `httpSites`, a display name,
category, disclosure level, optional domain affiliation, and lifecycle.

Each active service needs health checks or an explicit `exclusionReason`.
`planned`, `retired`, and `on-demand` services require a reason and are excluded
from continuous monitoring. An idle on-demand job is not a failed daemon.

## Health contracts

| Probe | Evidence |
| --- | --- |
| `http` | One owned site or endpoint, path, expected status, optional body assertions |
| `tcp` | Connection to the actual owned endpoint; `tcpProbe = false` is respected |
| `dns` | An A query to a declared host's DNS server with an expected IPv4 answer |
| `unit` | One loaded, active systemd unit |
| `job` | Successful normal exit of a service within `maxAgeSeconds`, observed since boot |
| `contract` | A caller-supplied evaluator for an existing named health contract |

Unit checks measure process state. HTTP `/` checks measure the served website.
Neither implies database readiness. Use protocol-specific readiness endpoints,
body assertions, or an existing application health contract for that claim.
Job evidence is deliberately conservative: missing evidence after reboot,
running jobs, signals, failed executions, and expired evidence are unhealthy.
For backup integrity, monitor the existing backup verifier job rather than infer
integrity from a running WAL receiver or a recent backup command alone.
Loopback endpoint checks owned by another host are automatically collected by
that host's publisher and rendered as external results, rather than probing a
relay or pretending the endpoint is LAN-reachable. Their resolved target is
literal loopback, HTTP bodies are size-bounded, and status/body/latency contracts
are preserved. HTTPS endpoint checks use a hostname-verified site instead.

Checks inherit profile visibility and category, with optional `visibility`,
`category`, and `displayName` overrides. An internal profile cannot publish a
public check. Public summaries require public HTTP sites. Diagnostic checks stay
internal even when the same service has a public summary.

## Nix adapter

```nix
fleetix.lib.gatus.inventory {
  inherit topology;
  hostName = "monitor";
  domains = ["example.org" "staff.example.org"];
  domain = "example.org";
  defaultDomain = "staff.example.org";
  includeInternal = false;
}
```

Domain inference matches DNS label boundaries and the longest configured suffix.
Mixed-domain services require an explicit affiliation. Domain-less services use
`defaultDomain`. `includeInternal = true` collects internal checks from every
domain, plus public checks affiliated with that instance's domain. The consumer
must authenticate that instance.

The result contains `endpoints`, `externalChecks`, `monitors`, and documented
`excluded` entries. `gatus.coverage` reports unprofiled endpoint/site additions.
The adapter validates raw Nix inputs too, including profiles not selected by the
current instance. Remote loopback probes become host-local external results.
For TLS endpoints requiring a named certificate, use a site check.

Gatus derives history keys from **category and display name**, not catalog IDs.
Keep those labels and the instance's SQLite state directory stable to retain
history. `gatus.key` follows Gatus v5.36's trimming, lowercasing, and punctuation
rules. The Nix adapter accepts ASCII labels because Nix cannot reproduce Go's
Unicode case folding. External-result keys must also be URL-safe: Gatus v5.36
does not unescape its route parameters. Monitor key collisions fail evaluation.

## External-result publisher library

`health::collect` accepts an explicit host, local-check selection, binary paths,
monotonic uptime, and a contract evaluator. It never discovers global fleet state.
The caller's contract evaluator receives a timeout budget. Deployment glue must
bound the publisher process, including any evaluator it supplies.

`health::publish` sends bearer-authenticated external results using the rendered
keys. Requests have timeouts, do not follow redirects or environment proxies,
and report bounded verdicts without probe output or secret paths. Both collection
and publication use batches of eight. An unavailable publisher is detected by
Gatus's external-endpoint heartbeat (three polling intervals).

Pass credentials at runtime. The Gatus configuration can contain an environment
placeholder; never put the credential itself in topology or a Nix derivation.

Tests: `cargo test --lib --test service_profiles --test health_publish` and
`tests/gatus.nix` evaluated with the flake's nixpkgs library.
