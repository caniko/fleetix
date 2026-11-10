//! Snapshots, diffs and consumer queries over a merged supply graph.

use super::{CONSUMERS_CONTRACT, DIFF_CONTRACT, GRAPH_CONTRACT, Graph, repository_of};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Per-usage change maps keyed by usage id, grouped by producer coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffReport {
    pub contract: &'static str,
    pub producers: Vec<ProducerDiff>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerDiff {
    pub coordinate: String,
    /// Usage id to the revisions it now pins.
    pub added: BTreeMap<String, Vec<String>>,
    /// Usage id to the revisions it used to pin.
    pub removed: BTreeMap<String, Vec<String>>,
    #[serde(rename = "revisionChanged")]
    pub revision_changed: BTreeMap<String, RevisionChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionChange {
    pub from: Vec<String>,
    pub to: Vec<String>,
}

type UsageRevisions = BTreeMap<String, BTreeMap<String, Vec<String>>>;

/// Compare the revisions each usage pins, grouped by the producer its `pins`
/// edges target. A usage with no `pins` edge groups under the empty coordinate.
pub fn diff(prev: &Graph, next: &Graph) -> DiffReport {
    let previous = usage_revisions(prev);
    let current = usage_revisions(next);
    let mut producers: BTreeMap<String, ProducerDiff> = BTreeMap::new();

    for (usage, next_producers) in &current {
        match previous.get(usage) {
            None => {
                for (producer, revisions) in next_producers {
                    entry(&mut producers, producer)
                        .added
                        .insert(usage.clone(), revisions.clone());
                }
            }
            Some(prev_producers) => {
                for (producer, to) in next_producers {
                    match prev_producers.get(producer) {
                        None => {
                            entry(&mut producers, producer)
                                .added
                                .insert(usage.clone(), to.clone());
                        }
                        Some(from) if from != to => {
                            entry(&mut producers, producer).revision_changed.insert(
                                usage.clone(),
                                RevisionChange {
                                    from: from.clone(),
                                    to: to.clone(),
                                },
                            );
                        }
                        Some(_) => {}
                    }
                }
                for (producer, from) in prev_producers {
                    if !next_producers.contains_key(producer) {
                        entry(&mut producers, producer)
                            .removed
                            .insert(usage.clone(), from.clone());
                    }
                }
            }
        }
    }
    for (usage, prev_producers) in &previous {
        if current.contains_key(usage) {
            continue;
        }
        for (producer, revisions) in prev_producers {
            entry(&mut producers, producer)
                .removed
                .insert(usage.clone(), revisions.clone());
        }
    }

    DiffReport {
        contract: DIFF_CONTRACT,
        producers: producers.into_values().collect(),
    }
}

fn entry<'a>(
    producers: &'a mut BTreeMap<String, ProducerDiff>,
    coordinate: &str,
) -> &'a mut ProducerDiff {
    producers
        .entry(coordinate.to_string())
        .or_insert_with(|| ProducerDiff {
            coordinate: coordinate.to_string(),
            ..ProducerDiff::default()
        })
}

fn usage_revisions(graph: &Graph) -> UsageRevisions {
    let mut usages = UsageRevisions::new();
    for node in graph.nodes.values().filter(|node| node.kind == "usage") {
        let mut producers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, edge) in graph.edges_of(&node.id, false) {
            if edge.kind != "pins" {
                continue;
            }
            if let Some((producer, revision)) = revision_target(&edge.to) {
                producers
                    .entry(producer.to_string())
                    .or_default()
                    .insert(revision.to_string());
            }
        }
        if producers.is_empty() {
            producers.insert(String::new(), BTreeSet::new());
        }
        usages.insert(
            node.id.clone(),
            producers
                .into_iter()
                .map(|(producer, revisions)| (producer, revisions.into_iter().collect()))
                .collect(),
        );
    }
    usages
}

/// `rev:<coordinate>@<revision>`, split from the right because a revision
/// never contains `@` while a scp-style coordinate may.
fn revision_target(target: &str) -> Option<(&str, &str)> {
    let (coordinate, revision) = target.strip_prefix("rev:")?.rsplit_once('@')?;
    Some((coordinate, revision))
}

/// One repository consuming a producer, as a work item for a landing engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerItem {
    pub coordinate: String,
    pub relative_path: Option<String>,
    pub usage: String,
    pub input: Option<String>,
    pub direct: Option<bool>,
    pub locked_revs: Vec<String>,
    pub target: Option<String>,
    pub behind: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumersReport {
    pub contract: &'static str,
    /// The canonical producer coordinate the query resolved to.
    pub producer: String,
    pub target: Option<String>,
    pub items: Vec<ConsumerItem>,
}

/// Every usage that pins `producer` (an alias resolves to its canonical
/// coordinate). `behind` is true when a target revision is given and the
/// usage does not pin it.
pub fn consumers(graph: &Graph, producer: &str, target: Option<&str>) -> ConsumersReport {
    let producer = resolve_producer(graph, producer);
    let mut items = Vec::new();
    for node in graph.nodes.values().filter(|node| node.kind == "usage") {
        let mut revisions = BTreeSet::new();
        for (_, edge) in graph.edges_of(&node.id, false) {
            if edge.kind != "pins" {
                continue;
            }
            if let Some((coordinate, revision)) = revision_target(&edge.to) {
                if normalize(coordinate) == producer {
                    revisions.insert(revision.to_string());
                }
            }
        }
        if revisions.is_empty() {
            continue;
        }
        let Some(coordinate) = repository_of(&node.id) else {
            continue;
        };
        let repository = graph.nodes.get(&format!("repo:{coordinate}"));
        let attribute = |key: &str| {
            repository
                .and_then(|repository| repository.attrs.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let locked_revs: Vec<_> = revisions.into_iter().collect();
        items.push(ConsumerItem {
            coordinate,
            relative_path: attribute("relativePath"),
            usage: node.id.clone(),
            input: attribute_of(node, "input"),
            direct: node
                .attrs
                .get("direct")
                .and_then(serde_json::Value::as_bool),
            behind: target.is_some_and(|target| !locked_revs.iter().any(|rev| rev == target)),
            locked_revs,
            target: target.map(str::to_string),
        });
    }
    ConsumersReport {
        contract: CONSUMERS_CONTRACT,
        producer,
        target: target.map(str::to_string),
        items,
    }
}

fn attribute_of(node: &super::GraphNode, key: &str) -> Option<String> {
    node.attrs
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn normalize(coordinate: &str) -> String {
    match coordinate.split_once('/') {
        Some((host, rest)) => format!("{}/{rest}", host.to_ascii_lowercase()),
        None => coordinate.to_ascii_lowercase(),
    }
}

/// A producer node's canonical coordinate, following its aliases.
fn resolve_producer(graph: &Graph, producer: &str) -> String {
    let normalized = normalize(producer);
    if graph.nodes.contains_key(&format!("producer:{normalized}")) {
        return normalized;
    }
    for (id, node) in &graph.nodes {
        if node.kind != "producer" {
            continue;
        }
        let aliases = node
            .attrs
            .get("aliases")
            .and_then(serde_json::Value::as_array);
        if aliases.is_some_and(|aliases| {
            aliases
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|alias| normalize(alias) == normalized)
        }) {
            if let Some(coordinate) = id.strip_prefix("producer:") {
                return normalize(coordinate);
            }
        }
    }
    normalized
}

/// The latest snapshot for a workspace root.
pub fn store_path(root: &Path) -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    state
        .join("fleetix/supply")
        .join(&digest[..16])
        .join("graph.json")
}

/// Load the latest snapshot, if one was stored for this root.
pub fn load_snapshot(root: &Path) -> miette::Result<Option<Graph>> {
    let path = store_path(root);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| miette::miette!("parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(miette::miette!("read {}: {error}", path.display())),
    }
}

/// Atomically replace the latest snapshot. The contract field must be the
/// graph contract so a later reader never mistakes another document for one.
pub fn save_snapshot(root: &Path, graph: &Graph) -> miette::Result<()> {
    if graph.contract != GRAPH_CONTRACT {
        return Err(miette::miette!(
            "refusing to store graph contract '{}', expected '{GRAPH_CONTRACT}'",
            graph.contract
        ));
    }
    let mut bytes = serde_json::to_vec_pretty(graph)
        .map_err(|error| miette::miette!("serialize supply graph: {error}"))?;
    bytes.push(b'\n');
    crate::fsutil::atomic_write(&store_path(root), &bytes)
}
