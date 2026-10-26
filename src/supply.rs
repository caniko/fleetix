//! Producer-neutral supply facts: which repositories consume which tools, and
//! whether an upgrade path covers each usage. See SUPPLY.md for the contract.
//!
//! Producers own their vocabularies (usage kinds, attributes, blocker codes).
//! Fleetix only merges their documents and applies the generic gap rules.

use crate::topology::{Supply, Topology};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const CONTRACT: &str = "fleetix.supply-facts.v1";
pub const GAPS_CONTRACT: &str = "fleetix.supply-gaps.v1";
pub const PATH_CONTRACT: &str = "fleetix.supply-path.v1";

pub type Attrs = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactsDocument {
    pub contract: String,
    pub vocabulary: String,
    pub producer: Producer,
    pub observed_at: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub errors: Vec<FactError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub attrs: Attrs,
    #[serde(default)]
    pub expects: Vec<Expect>,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
}

/// An attribute some producer must supply; while it is missing or null the
/// node reports `blocker`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub attr: String,
    pub blocker: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blocker {
    pub code: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default)]
    pub attrs: Attrs,
}

/// One row of a producer's upgrade-path matrix. `match` keys are compared
/// with the owning repository node's string attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capability {
    pub usage_kind: String,
    #[serde(rename = "match", default)]
    pub matches: BTreeMap<String, String>,
    pub path: String,
    pub via: String,
    pub lands: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    pub status: CapabilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityStatus {
    Implemented,
    Planned,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactError {
    pub subject: String,
    pub code: String,
    pub message: String,
}

/// Parse one facts document or a JSON array of them, rejecting other contracts.
pub fn parse(json: &str) -> miette::Result<Vec<FactsDocument>> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| miette::miette!("parse facts: {error}"))?;
    let docs: Vec<FactsDocument> = if value.is_array() {
        serde_json::from_value(value)
    } else {
        serde_json::from_value(value).map(|doc| vec![doc])
    }
    .map_err(|error| miette::miette!("parse facts: {error}"))?;
    if let Some(doc) = docs.iter().find(|doc| doc.contract != CONTRACT) {
        return Err(miette::miette!(
            "unsupported facts contract '{}', expected '{CONTRACT}'",
            doc.contract
        ));
    }
    Ok(docs)
}

/// A value together with the producer that declared it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sourced<T> {
    pub producer: String,
    #[serde(flatten)]
    pub value: T,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub attrs: Attrs,
    pub expects: Vec<Sourced<Expect>>,
    pub blockers: Vec<Sourced<Blocker>>,
    pub producers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub attrs: Attrs,
    pub producers: Vec<String>,
}

/// Merged facts with adjacency by node id.
#[derive(Debug, Default, Serialize)]
pub struct Graph {
    pub nodes: BTreeMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub capabilities: Vec<Sourced<Capability>>,
    pub errors: Vec<Sourced<FactError>>,
    /// Contradictory facts found while merging (R1).
    pub conflicts: Vec<Gap>,
    #[serde(skip)]
    outgoing: BTreeMap<String, Vec<usize>>,
    #[serde(skip)]
    incoming: BTreeMap<String, Vec<usize>>,
    /// The topology supply runner and cache nodes join (R6), when given.
    #[serde(skip)]
    supply: Option<Supply>,
}

/// Merge documents by node id and edge `(from, to, kind)` (R1). Without a
/// topology, runner and cache nodes are not joined (R6).
pub fn build(docs: &[FactsDocument], topology: Option<&Topology>) -> Graph {
    let mut graph = Graph {
        supply: topology.map(|topology| topology.supply.clone()),
        ..Graph::default()
    };
    let mut edge_index: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for doc in docs {
        let producer = &doc.producer.name;
        for node in &doc.nodes {
            graph.merge_node(producer, node);
        }
        for edge in &doc.edges {
            let key = (edge.from.clone(), edge.to.clone(), edge.kind.clone());
            if let Some(&index) = edge_index.get(&key) {
                let merged = &mut graph.edges[index];
                let conflicts = merge_attrs(&mut merged.attrs, &edge.attrs, &merged.producers);
                add_producer(&mut merged.producers, producer);
                let subject = format!("{} edge {} -> {}", edge.kind, edge.from, edge.to);
                graph.record_conflicts(&edge.from, &subject, producer, conflicts);
                continue;
            }
            let index = graph.edges.len();
            edge_index.insert(key, index);
            graph.edges.push(GraphEdge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                kind: edge.kind.clone(),
                attrs: without_nulls(&edge.attrs),
                producers: vec![producer.clone()],
            });
            graph
                .outgoing
                .entry(edge.from.clone())
                .or_default()
                .push(index);
            graph
                .incoming
                .entry(edge.to.clone())
                .or_default()
                .push(index);
        }
        for capability in &doc.capabilities {
            push_unique(&mut graph.capabilities, producer, capability);
        }
        for error in &doc.errors {
            push_unique(&mut graph.errors, producer, error);
        }
    }
    graph
}

impl Graph {
    fn merge_node(&mut self, producer: &str, node: &Node) {
        let Some(merged) = self.nodes.get_mut(&node.id) else {
            self.nodes.insert(
                node.id.clone(),
                GraphNode {
                    id: node.id.clone(),
                    kind: node.kind.clone(),
                    attrs: without_nulls(&node.attrs),
                    expects: Vec::new(),
                    blockers: Vec::new(),
                    producers: vec![producer.to_string()],
                },
            );
            return self.merge_node_lists(producer, node);
        };
        let mut conflicts = merge_attrs(&mut merged.attrs, &node.attrs, &merged.producers);
        if merged.kind != node.kind {
            conflicts.push(format!(
                "kind '{}' ({}) vs '{}'",
                merged.kind,
                merged.producers.join(", "),
                node.kind
            ));
        }
        add_producer(&mut merged.producers, producer);
        self.record_conflicts(&node.id, &node.id, producer, conflicts);
        self.merge_node_lists(producer, node);
    }

    fn merge_node_lists(&mut self, producer: &str, node: &Node) {
        let merged = self.nodes.get_mut(&node.id).expect("node merged above");
        for expect in &node.expects {
            if !merged.expects.iter().any(|known| {
                known.value.attr == expect.attr && known.value.blocker == expect.blocker
            }) {
                merged.expects.push(sourced(producer, expect));
            }
        }
        for blocker in &node.blockers {
            push_unique(&mut merged.blockers, producer, blocker);
        }
    }

    fn record_conflicts(&mut self, node: &str, subject: &str, producer: &str, found: Vec<String>) {
        for conflict in found {
            self.conflicts.push(Gap {
                repository: repository_of(node),
                node: node.to_string(),
                usage_kind: None,
                blocker: "contract-conflict".into(),
                subcode: None,
                reason: format!("{subject}: {conflict} ({producer})"),
                owner: None,
                source: "merge",
            });
        }
    }

    fn edges_of(&self, id: &str, reverse: bool) -> impl Iterator<Item = (usize, &GraphEdge)> {
        let index = if reverse {
            &self.incoming
        } else {
            &self.outgoing
        };
        index
            .get(id)
            .into_iter()
            .flatten()
            .map(|&i| (i, &self.edges[i]))
    }

    fn contains(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
            || self.outgoing.contains_key(id)
            || self.incoming.contains_key(id)
    }

    /// The most specific matching row per producer (R2); ties keep every row.
    fn best_rows(
        &self,
        usage_kind: &str,
        repo: Option<&GraphNode>,
    ) -> BTreeMap<&str, Vec<&Capability>> {
        let mut best: BTreeMap<&str, (usize, Vec<&Capability>)> = BTreeMap::new();
        for row in &self.capabilities {
            let capability = &row.value;
            let applies = capability.usage_kind == usage_kind
                && capability.matches.iter().all(|(key, expected)| {
                    repo.and_then(|repo| repo.attrs.get(key))
                        .and_then(Value::as_str)
                        == Some(expected.as_str())
                });
            if !applies {
                continue;
            }
            let specificity = capability.matches.len();
            let entry = best
                .entry(row.producer.as_str())
                .or_insert((specificity, Vec::new()));
            if specificity > entry.0 {
                *entry = (specificity, vec![capability]);
            } else if specificity == entry.0 {
                entry.1.push(capability);
            }
        }
        best.into_iter()
            .map(|(producer, (_, rows))| (producer, rows))
            .collect()
    }

    /// Whether any producer's best row lands this usage; otherwise why not.
    fn coverage(&self, node: &GraphNode, gap: &dyn Fn(Draft) -> Gap) -> Result<(), Vec<Gap>> {
        let Some(usage_kind) = node.attrs.get("usageKind").and_then(Value::as_str) else {
            return Err(vec![gap(Draft::new(
                "no-capability",
                "usage node has no usageKind attribute",
                None,
                "capability",
            ))]);
        };
        let repo_id = repository_of(&node.id).map(|coordinate| format!("repo:{coordinate}"));
        let repo = repo_id.as_deref().and_then(|id| self.nodes.get(id));
        let rows = self.best_rows(usage_kind, repo);
        if rows.is_empty() {
            return Err(vec![gap(Draft::new(
                "no-capability",
                format!("no capability row covers usage kind '{usage_kind}'"),
                None,
                "capability",
            ))]);
        }
        let mut failures = Vec::new();
        for (producer, rows) in rows {
            let [row] = rows.as_slice() else {
                let paths: Vec<_> = rows.iter().map(|row| row.path.as_str()).collect();
                failures.push(gap(Draft::new(
                    "contract-conflict",
                    format!(
                        "{} equally specific '{usage_kind}' rows: {}",
                        rows.len(),
                        paths.join(", ")
                    ),
                    Some(producer),
                    "capability",
                )));
                continue;
            };
            match self.covers(row, producer, repo_id.as_deref()) {
                Ok(()) => return Ok(()),
                Err(draft) => failures.push(gap(draft)),
            }
        }
        Err(failures)
    }

    /// R3 (implemented and lands) then R4 (enabled enrollment).
    fn covers(&self, row: &Capability, producer: &str, repo_id: Option<&str>) -> Result<(), Draft> {
        if row.status != CapabilityStatus::Implemented || !row.lands {
            let reason = match &row.fallback {
                Some(fallback) => format!("{} (fallback: {fallback})", row.reason),
                None => row.reason.clone(),
            };
            let blocker = row.blocker.as_deref().unwrap_or("no-capability");
            return Err(Draft::new(blocker, reason, Some(producer), "capability"));
        }
        let Some(orchestrator) = &row.orchestrator else {
            return Ok(());
        };
        let target = format!("orchestrator:{orchestrator}");
        let prefix = format!("{target}/");
        let enrollments: Vec<_> = repo_id
            .into_iter()
            .flat_map(|id| self.edges_of(id, false))
            .map(|(_, edge)| edge)
            .filter(|edge| {
                edge.kind == "enrolledIn" && (edge.to == target || edge.to.starts_with(&prefix))
            })
            .collect();
        if enrollments
            .iter()
            .any(|edge| edge.attrs.get("enabled") == Some(&Value::Bool(true)))
        {
            return Ok(());
        }
        let Some(disabled) = enrollments.first() else {
            return Err(Draft::new(
                "not-enrolled",
                format!("not enrolled in any {orchestrator} orchestrator"),
                None,
                "enrollment",
            ));
        };
        let text = |key: &str| disabled.attrs.get(key).and_then(Value::as_str);
        let mut draft = Draft::new(
            "operator-disabled",
            text("reason").unwrap_or("disabled by the operator"),
            disabled.producers.first().map(String::as_str),
            "enrollment",
        );
        draft.subcode = text("blocker").map(str::to_string);
        Err(draft)
    }

    /// R6: a runner node needs a topology pool offering its label on its
    /// forge host, and a cache node a topology cache of its name.
    fn topology_gap(&self, node: &GraphNode) -> Option<Draft> {
        let supply = self.supply.as_ref()?;
        let (blocker, joined) = match node.kind.as_str() {
            "runner" => (
                "unknown-runner",
                node.id
                    .strip_prefix("runner:")
                    .and_then(|id| id.split_once('/'))
                    .is_some_and(|(host, label)| {
                        supply.runner_pools.values().any(|pool| {
                            pool.forge_host == host
                                && pool.labels.iter().any(|known| known == label)
                        })
                    }),
            ),
            "cache" => (
                "unknown-cache",
                node.id
                    .strip_prefix("cache:")
                    .is_some_and(|name| supply.caches.contains_key(name)),
            ),
            _ => return None,
        };
        (!joined).then(|| {
            Draft::new(
                blocker,
                format!("topology supply does not declare {}", node.id),
                None,
                "topology",
            )
        })
    }
}

/// A gap before it is bound to its node.
#[derive(Clone)]
struct Draft {
    blocker: String,
    subcode: Option<String>,
    reason: String,
    owner: Option<String>,
    source: &'static str,
}

impl Draft {
    fn new(
        blocker: &str,
        reason: impl Into<String>,
        owner: Option<&str>,
        source: &'static str,
    ) -> Self {
        Self {
            blocker: blocker.to_string(),
            subcode: None,
            reason: reason.into(),
            owner: owner.map(str::to_string),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub repository: Option<String>,
    pub node: String,
    pub usage_kind: Option<String>,
    pub blocker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcode: Option<String>,
    pub reason: String,
    pub owner: Option<String>,
    /// The rule family: merge, capability, enrollment, expectation, topology
    /// or producer.
    pub source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GapReport {
    pub contract: &'static str,
    pub summary: Summary,
    pub gaps: Vec<Gap>,
    pub errors: Vec<Sourced<FactError>>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub repositories: usize,
    /// Usages considered: every usage node except `state: absent`.
    pub usages: usize,
    /// Usages that some producer's upgrade path lands (R2-R4).
    pub covered: usize,
    pub drift: DriftSummary,
    pub by_blocker: Vec<BlockerCount>,
    pub by_repository: Vec<RepositoryCount>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DriftSummary {
    pub covered: usize,
    pub uncovered: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockerCount {
    pub blocker: String,
    pub gaps: usize,
    pub repositories: usize,
    pub usages: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryCount {
    pub repository: String,
    pub gaps: usize,
    pub blockers: Vec<String>,
}

/// Apply the gap rules R2-R7 to every node.
pub fn gaps(graph: &Graph) -> GapReport {
    let mut gaps = graph.conflicts.clone();
    let mut summary = Summary {
        repositories: graph
            .nodes
            .values()
            .filter(|node| node.kind == "repo")
            .count(),
        ..Summary::default()
    };
    for node in graph.nodes.values() {
        let state = node.attrs.get("state").and_then(Value::as_str);
        let is_usage = node.kind == "usage";
        if is_usage && state == Some("absent") {
            continue;
        }
        let usage_kind = is_usage
            .then(|| node.attrs.get("usageKind").and_then(Value::as_str))
            .flatten();
        let gap = |draft: Draft| Gap {
            repository: repository_of(&node.id),
            node: node.id.clone(),
            usage_kind: usage_kind.map(str::to_string),
            blocker: draft.blocker,
            subcode: draft.subcode,
            reason: draft.reason,
            owner: draft.owner,
            source: draft.source,
        };
        for blocker in &node.blockers {
            let owner = blocker.value.owner.as_deref().unwrap_or(&blocker.producer);
            gaps.push(gap(Draft::new(
                &blocker.value.code,
                blocker.value.reason.clone(),
                Some(owner),
                "producer",
            )));
        }
        for expect in &node.expects {
            if !node.attrs.contains_key(&expect.value.attr) {
                gaps.push(gap(Draft::new(
                    &expect.value.blocker,
                    expect.value.reason.clone(),
                    Some(&expect.producer),
                    "expectation",
                )));
            }
        }
        if let Some(draft) = graph.topology_gap(node) {
            // One gap per repository that depends on the unjoined node.
            let dependents: BTreeSet<_> = graph
                .edges_of(&node.id, true)
                .filter_map(|(_, edge)| repository_of(&edge.from))
                .collect();
            if dependents.is_empty() {
                gaps.push(gap(draft));
            } else {
                for repository in dependents {
                    gaps.push(Gap {
                        repository: Some(repository),
                        ..gap(draft.clone())
                    });
                }
            }
        }
        if !is_usage {
            continue;
        }
        summary.usages += 1;
        let covered = match graph.coverage(node, &gap) {
            Ok(()) => true,
            Err(failures) => {
                gaps.extend(failures);
                false
            }
        };
        summary.covered += usize::from(covered);
        if state == Some("drift") {
            if covered {
                summary.drift.covered += 1;
            } else {
                summary.drift.uncovered += 1;
            }
        }
    }
    gaps.sort();
    gaps.dedup();

    let mut by_blocker: BTreeMap<&str, (usize, BTreeSet<&str>, BTreeSet<&str>)> = BTreeMap::new();
    let mut by_repository: BTreeMap<&str, (usize, BTreeSet<&str>)> = BTreeMap::new();
    for gap in &gaps {
        let entry = by_blocker.entry(&gap.blocker).or_default();
        entry.0 += 1;
        if let Some(repository) = &gap.repository {
            entry.1.insert(repository);
            let repo = by_repository.entry(repository).or_default();
            repo.0 += 1;
            repo.1.insert(&gap.blocker);
        }
        if gap.usage_kind.is_some() {
            entry.2.insert(&gap.node);
        }
    }
    summary.by_blocker = by_blocker
        .into_iter()
        .map(|(blocker, (gaps, repositories, usages))| BlockerCount {
            blocker: blocker.to_string(),
            gaps,
            repositories: repositories.len(),
            usages: usages.len(),
        })
        .collect();
    summary
        .by_blocker
        .sort_by(|a, b| b.gaps.cmp(&a.gaps).then_with(|| a.blocker.cmp(&b.blocker)));
    summary.by_repository = by_repository
        .into_iter()
        .map(|(repository, (gaps, blockers))| RepositoryCount {
            repository: repository.to_string(),
            gaps,
            blockers: blockers.into_iter().map(str::to_string).collect(),
        })
        .collect();

    GapReport {
        contract: GAPS_CONTRACT,
        summary,
        errors: graph.errors.clone(),
        gaps,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathReport {
    pub contract: &'static str,
    pub root: String,
    pub reverse: bool,
    pub depth: usize,
    pub nodes: Vec<PathNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathNode {
    pub id: String,
    pub kind: Option<String>,
    pub depth: usize,
    pub attrs: Attrs,
}

/// Breadth-first walk from `id` (or `repo:<id>` for a bare coordinate),
/// following edges forward, or backward with `reverse`, up to `depth` hops.
pub fn walk(graph: &Graph, id: &str, reverse: bool, depth: usize) -> miette::Result<PathReport> {
    let root = [id.to_string(), format!("repo:{id}")]
        .into_iter()
        .find(|candidate| graph.contains(candidate))
        .ok_or_else(|| miette::miette!("unknown supply node '{id}'"))?;
    let mut seen = BTreeSet::from([root.clone()]);
    let mut queue = VecDeque::from([(root.clone(), 0)]);
    let mut edges = BTreeSet::new();
    let mut nodes = Vec::new();
    while let Some((current, hops)) = queue.pop_front() {
        let known = graph.nodes.get(&current);
        nodes.push(PathNode {
            id: current.clone(),
            kind: known.map(|node| node.kind.clone()),
            depth: hops,
            attrs: known.map(|node| node.attrs.clone()).unwrap_or_default(),
        });
        if hops == depth {
            continue;
        }
        for (index, edge) in graph.edges_of(&current, reverse) {
            edges.insert(index);
            let next = if reverse { &edge.from } else { &edge.to };
            if seen.insert(next.clone()) {
                queue.push_back((next.clone(), hops + 1));
            }
        }
    }
    Ok(PathReport {
        contract: PATH_CONTRACT,
        root,
        reverse,
        depth,
        nodes,
        edges: edges
            .into_iter()
            .map(|index| graph.edges[index].clone())
            .collect(),
    })
}

/// The repository coordinate of a `repo:` or `usage:` node id.
pub fn repository_of(id: &str) -> Option<String> {
    id.strip_prefix("repo:")
        .or_else(|| {
            id.strip_prefix("usage:")
                .and_then(|rest| rest.split_once('#'))
                .map(|(coordinate, _)| coordinate)
        })
        .map(str::to_string)
}

fn sourced<T: Clone>(producer: &str, value: &T) -> Sourced<T> {
    Sourced {
        producer: producer.to_string(),
        value: value.clone(),
    }
}

fn push_unique<T: Clone + PartialEq>(list: &mut Vec<Sourced<T>>, producer: &str, value: &T) {
    if !list
        .iter()
        .any(|known| known.producer == producer && &known.value == value)
    {
        list.push(sourced(producer, value));
    }
}

fn add_producer(producers: &mut Vec<String>, producer: &str) {
    if !producers.iter().any(|known| known == producer) {
        producers.push(producer.to_string());
    }
}

/// Null means "unknown": it neither sets nor contradicts an attribute.
fn without_nulls(attrs: &Attrs) -> Attrs {
    attrs
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Add new attributes; a different non-null value for a known key is a
/// conflict and keeps the first value.
fn merge_attrs(into: &mut Attrs, from: &Attrs, producers: &[String]) -> Vec<String> {
    let mut conflicts = Vec::new();
    for (key, value) in without_nulls(from) {
        match into.get(&key) {
            Some(known) if *known != value => conflicts.push(format!(
                "attr '{key}' {known} ({}) vs {value}",
                producers.join(", ")
            )),
            Some(_) => {}
            None => {
                into.insert(key, value);
            }
        }
    }
    conflicts
}
