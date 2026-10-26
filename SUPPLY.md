# Supply facts

`fleetix::supply` merges versioned supply-chain facts into one graph and reports
which upgrade paths cover each usage, and why the uncovered ones need work.
Facts are runtime observations, not topology: producers write them, fleetix
merges them, and nothing is committed to the topology.

Fleetix owns the envelope (`fleetix.supply-facts.v1`) and the generic rules.
Producers own their vocabularies: usage kinds, attributes and blocker codes.
For example, Simit emits `simit.usage-facts.v1` with its capability matrix, and
a fleet operator emits an overlay (`canix.supply-overlay.v1`) with enrollment
policy and receipts. Fleetix never interprets a producer's usage kinds.

## Document

Every document is one JSON object. `fleetix::supply::parse` also accepts a JSON
array of documents. Unknown fields and any other `contract` value are rejected,
so a reader never misreads a newer contract.

```json
{
  "contract": "fleetix.supply-facts.v1",
  "vocabulary": "simit.usage-facts.v1",
  "producer": {"name": "simit", "version": "0.21.0"},
  "observedAt": "2026-10-26T06:00:00Z",
  "nodes": [],
  "edges": [],
  "capabilities": [],
  "errors": []
}
```

| Item | Fields |
| --- | --- |
| node | `id`, `kind`, `attrs` (object), `expects[{attr, blocker, reason}]`, `blockers[{code, reason, owner?}]` |
| edge | `from`, `to`, `kind`, `attrs` |
| capability | `usageKind`, `match` (string map), `path`, `via`, `lands`, `orchestrator?`, `status` (`implemented`, `planned` or `unsupported`), `blocker?`, `reason`, `fallback?` |
| error | `subject` (a node id or input path), `code`, `message` |

`observedAt` is RFC 3339 UTC. `nodes`, `edges`, `capabilities` and `errors`
default to empty, and `attrs`, `expects` and `blockers` too.

### Node ids

| Id | Kind |
| --- | --- |
| `repo:<coordinate>` | `repo`; a coordinate is `host/owner/name` and never contains `#` |
| `usage:<coordinate>#<usageKind>[:<locator>]` | `usage`; the repository is everything before `#` |
| `rev:<source>@<revision>` | `rev` |
| `runner:<forgeHost>/<label>` | `runner` |
| `cache:<name>` | `cache` |
| `orchestrator:<kind>/<name>` | `orchestrator` |

Fleetix reads only these attributes and edges:

- `usage` nodes: `attrs.usageKind` (required) and `attrs.state` (`current`,
  `drift`, `hand-rolled`, `absent` or `unknown`). Usages in state `absent` are
  skipped.
- `repo` nodes: the string attributes named by capability `match` keys, such as
  `forge`, `ciTarget` and `landing`.
- `enrolledIn` edges from a `repo` to an `orchestrator`, with `attrs.enabled`
  and optional `attrs.reason` and `attrs.blocker`. Other edge kinds, such as
  `uses`, `contains`, `pins`, `runsOn` and `via`, can be walked but carry no
  rule.

## Topology supply

The topology declares the shared supply that facts join against: runner pools
and binary caches. Each can name the `services.catalog` entry that operates it.
Forges and repositories never enter topology.

```pkl
supply = new S.Supply {
  runnerPools = new {
    ["builder-nix"] = new S.RunnerPool {
      forgeHost = "git.example.test"
      labels = new Listing { "builder-nix" }
      service = "runner-nix"
    }
  }
  caches = new {
    ["fleet"] = new S.BinaryCache { url = "https://cache.example.test/fleet"; service = "cache" }
  }
}
```

Validation reports `supply.unknown_service` for a service missing from the
catalog, and `supply.duplicate_runner_label` when one forge host's label is
declared more than once.

## Rules

- **R1 merge.** Nodes merge by id, and edges by `(from, to, kind)`. A null
  attribute means unknown: it neither sets nor contradicts a value. A different
  kind, or a different non-null value for a known attribute, keeps the first
  value and reports `contract-conflict`. Expectations, blockers, capabilities
  and errors are deduplicated.
- **R2 selection.** For each usage and each producer, the capability rows with
  the usage's kind whose `match` entries all equal the owning repository's
  attributes apply. The row with the most `match` keys wins. A tie between
  different rows is `contract-conflict`.
- **R3 coverage.** A winning row covers the usage only when it is `implemented`
  and `lands`. Otherwise the gap is the row's `blocker` (`no-capability` when it
  has none), and `fallback` is appended to the reason. A usage that no row
  applies to is `no-capability`.
- **R4 enrollment.** A covering row that names an `orchestrator` kind also needs
  an `enrolledIn` edge from the repository to `orchestrator:<kind>/<name>` with
  `enabled: true`. A disabled or unflagged edge is `operator-disabled`, with the
  edge's `reason` and its `blocker` as `subcode`. No edge is `not-enrolled`.
- **R5 expectations.** An expectation whose attribute is still missing or null
  after merging reports its blocker. Another producer satisfies it by setting
  the attribute on the same node.
- **R6 topology.** When `build` receives a topology, runner and cache nodes join
  its `supply` section. `runner:<forgeHost>/<label>` needs a runner pool on that
  forge host that offers the label, and `cache:<name>` needs that cache.
  Otherwise the node reports `unknown-runner` or `unknown-cache` once for each
  repository with an edge to it, or once without a repository when none has.
  Without a topology, nothing is joined.
- **R7 pass-through.** Node blockers become gaps. Their owner defaults to the
  producer.

A usage is covered when any producer's winning row covers it. Only an uncovered
usage reports its rows' failures, one gap per producer.

## Reports

`gaps` returns `fleetix.supply-gaps.v1`:

- `summary`: repository and usage counts, `covered`, and `drift` split into
  `covered` and `uncovered`. `byBlocker` gives the distinct repositories and
  usages per blocker. `byRepository` gives each repository's blockers.
- `gaps[]`: `{repository, node, usageKind, blocker, subcode?, reason, owner,
  source}`. `source` is the rule family: `merge`, `capability`, `enrollment`,
  `expectation`, `topology` or `producer`. `owner` is the producer whose fact
  failed, or a blocker's declared owner.
- `errors[]`: producer errors, passed through with their producer.

`walk` returns `fleetix.supply-path.v1`: a breadth-first walk from a node id,
or from `repo:<id>` for a bare coordinate. It follows edges forward, or backward
with `reverse`, up to `depth` hops. A reverse walk from a `rev:` node answers
"who pins this revision". From a `runner:` node it answers "what depends on
this runner".

## Recommended blocker vocabulary

Producers may define their own codes. These are the shared meanings.

| Blocker | Meaning | Usual source |
| --- | --- | --- |
| `adapter-missing`, `pr-required-unsupported`, `provider-mismatch`, `scope-unsupported` | The best path is not implemented | producer matrix |
| `no-orchestrator` | Implemented, but nothing lands it | producer matrix |
| `manual-pin`, `transitive-pin`, `unpinned-runtime` | Flake inputs, repository variables, command-string pins, floating references | producer matrix |
| `config-unresolved`, `config-not-persistable`, `config-conflict` | Configuration source problems | producer node |
| `unknown-generator-revision` | No receipt or pin proves the generator revision | producer expectation, overlay |
| `not-enrolled`, `operator-disabled` | Enrollment policy | R4 and the operator overlay |
| `unknown-runner`, `unknown-cache` | The topology join failed | R6 |
| `generator-pin-skew`, `snapshot-unavailable` | Orchestrator or snapshot problems | operator overlay |
| `no-capability`, `contract-conflict` | Holes or contradictions in the facts | R1-R3 |

Operator `subcode` values for `operator-disabled` include `provider-mismatch`,
`pr-required`, `credential-denied`, `ruleset-unsupported`,
`plan-admission-unsupported`, `campaign-hold`, `config-error`,
`config-not-persisted`, `actions-disabled`, `local-edits-unreconciled`,
`repository-unready` and `covered-by-profile`.
