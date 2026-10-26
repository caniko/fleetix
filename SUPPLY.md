| `workflow:<coordinate>#<path>` | `workflow`; one CI workflow file |
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
| `workflow:<coordinate>#<path>` | `workflow`; one CI workflow file |

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

The topology declares the shared supply that facts join against: runner pools,
binary caches and agent-minute budgets. A pool or cache can name the
`services.catalog` entry that operates it. `metered` marks a pool whose minutes
are billed. A budget gives a monthly allowance shared by its pools. Forges and
repositories never enter topology.

```pkl
supply = new S.Supply {
  runnerPools = new {
    ["builder-nix"] = new S.RunnerPool {
      forgeHost = "git.example.test"
      labels = new Listing { "builder-nix" }
      service = "runner-nix"
      metered = true
    }
  }
  caches = new {
    ["fleet"] = new S.BinaryCache { url = "https://cache.example.test/fleet"; service = "cache" }
  }
  budgets = new {
    ["builder"] = new S.Budget { pools = new Listing { "builder-nix" }; minutesPerMonth = 2500 }
  }
}
```

Validation reports `supply.unknown_service` for a service missing from the
catalog, `supply.duplicate_runner_label` when one forge host's label is
declared more than once, and `supply.unknown_budget_pool` for a budget pool
that is not declared.

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

## Budgets

`budget` returns `fleetix.supply-budget.v1` with one forecast per topology
budget. It reads `workflow:<coordinate>#<path>` nodes that have a `runsOn` edge
to a runner of one of the budget's pools:

- `attrs.observed`: `{windowDays, runs, agentMinutes, byLane, failedMinutes,
  killedMinutes}`, for example from an operator's CI observation overlay.
  `byLane` maps lanes such as `push:default`, `push:branch` and `pull_request`
  to minutes. A workflow without `observed` adds nothing.
- `attrs.triggers`: `{events, branches}`, the static triggers, for example
  from Simit. Empty `branches` means push runs on any branch.

`observedMinutes` scales each workflow to 30 days. `updaterMinutes` adds
planned qualification runs: a workflow on a metered pool whose repository has
an enabled `enrolledIn` edge to the given orchestrator kind costs
`runsPerUpdate` × its average minutes per run × updates per month.
`runsPerUpdate` is an attribute of the orchestrator node and defaults to 2.
`forecastMinutes` is their sum. `utilization` is the forecast divided by
`minutesPerMonth`.

Candidates are ranked by estimated savings. One workflow's estimates can
overlap, so don't add them up.

| Rule | Applies when | Savings |
| --- | --- | --- |
| `duplicate-trigger` | Triggers include push and pull_request, and push isn't limited (no branches, or a `*` pattern) | `push:branch` minutes |
| `heavy-on-pr` | The workflow averages more than 30 minutes per run | `pull_request` minutes |
| `failure-waste` | Any failed runs | `failedMinutes` |
| `timeout-waste` | Any killed or timed-out runs | `killedMinutes` |

## CLI

```bash
fleetix supply gaps --facts F... [--topology P] [--by blocker|repo] [--format human|json] [--max-age-days N]
fleetix supply path <node|coordinate> --facts F... [--reverse] [--depth N] [--format human|json]
fleetix supply graph --facts F...
fleetix supply budget --facts F... --topology P [--updates-per-month N] [--orchestrator KIND] [--format human|json]
```

Each `--facts` file holds one document or a JSON array of documents.
`--topology` loads and validates a Pkl topology for R6 and budgets.
`--max-age-days` warns on stderr about documents observed longer ago than that.
Human output is Markdown tables. `--format json` prints the
`fleetix.supply-gaps.v1`, `fleetix.supply-path.v1` or
`fleetix.supply-budget.v1` report. `graph` prints the merged graph, including
merge conflicts, as `fleetix.supply-graph.v1` JSON.

Exit codes: 2 for a document that is not valid facts or a budget without a
topology, 3 for an unreadable file, 4 for an unknown `path` node, and 1 for a
topology that fails to evaluate.

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
