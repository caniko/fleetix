//! Dependency-aware construction shared by independent deployment attempts.
//!
//! This engine neither evaluates flakes nor activates machines. A backend supplies
//! an exact output graph; frontends retain their source, credentials and safety
//! stages. One train has one immutable builder/store policy. All clocks are supplied
//! by the caller, making fairness and restart behavior reproducible.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[cfg(all(unix, feature = "build-train-runtime"))]
pub mod runtime;

/// Version of the durable state and coordinator protocol.
pub const VERSION: u32 = 1;

/// Exact backend derivation and named output, never a package name or revision.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Goal {
    pub derivation: String,
    pub output: String,
}

impl fmt::Display for Goal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}^{}", self.derivation, self.output)
    }
}

impl From<Goal> for String {
    fn from(value: Goal) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for Goal {
    type Error = String;
    fn try_from(value: String) -> Result<Self, String> {
        let (derivation, output) = value.split_once('^').ok_or("goal must name an output")?;
        if derivation.is_empty() || output.is_empty() || output.contains('^') {
            return Err("invalid goal identity".into());
        }
        Ok(Self {
            derivation: derivation.into(),
            output: output.into(),
        })
    }
}

/// Restore-only work cannot fall through to source compilation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Build,
    Restore,
}

/// Immutable backend evidence plus the current realization plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub output_path: String,
    pub dependencies: BTreeSet<Goal>,
    pub operation: Operation,
}

/// Full static output graph. Restore nodes prune their build-only dependencies.
pub type Graph = BTreeMap<Goal, Definition>;

/// Request-scoped identity. Adding train membership never rewrites an attempt ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub attempt: String,
    pub target: String,
    pub source: String,
    pub roots: BTreeSet<Goal>,
    pub activates: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestRecord {
    pub request: Request,
    pub sequence: u64,
    pub cancelled: bool,
    pub admitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    Pending,
    Running { serial: u64 },
    Complete,
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub definition: Definition,
    pub state: NodeState,
    pub queued_at: u64,
}

/// Receipt for one dispatched goal. Old generations cannot acknowledge new work.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dispatch {
    pub goal: Goal,
    pub definition: Definition,
    pub serial: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Pending,
    Ready,
    Cancelled,
    Failed(String),
}

/// Durable fence survives both client disconnect and coordinator replacement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fence {
    pub token: String,
    pub attempt: String,
}

/// Builder-local state. Mutation is serialized by the coordinator, not clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Train {
    pub version: u32,
    pub policy: String,
    pub requests: BTreeMap<String, RequestRecord>,
    pub nodes: BTreeMap<Goal, Node>,
    pub fence: Option<Fence>,
    /// After this bounded wait, oldest-ready wins over shared-first.
    pub aging_seconds: u64,
    sequence: u64,
    dispatch_serial: u64,
    latest_activation: BTreeMap<String, String>,
}

impl Train {
    pub fn new(policy: String, aging_seconds: u64) -> Self {
        Self {
            version: VERSION,
            policy,
            requests: BTreeMap::new(),
            nodes: BTreeMap::new(),
            fence: None,
            aging_seconds,
            sequence: 0,
            dispatch_serial: 0,
            latest_activation: BTreeMap::new(),
        }
    }

    /// Validate the entire join before mutating durable state.
    pub fn submit(&mut self, request: Request, graph: Graph, now: u64) -> Result<(), String> {
        if request.attempt.is_empty()
            || request.target.is_empty()
            || request.source.is_empty()
            || request.roots.is_empty()
        {
            return Err("request identity and roots must be nonempty".into());
        }
        validate_graph(&request.roots, &graph)?;
        if let Some(previous) = self.requests.get(&request.attempt) {
            if previous.request != request {
                return Err("attempt has a different frozen identity".into());
            }
        }
        for (goal, definition) in &graph {
            if let Some(previous) = self.nodes.get(goal) {
                if previous.definition.output_path != definition.output_path
                    || previous.definition.dependencies != definition.dependencies
                {
                    return Err(format!("conflicting backend evidence for {goal}"));
                }
            }
        }
        for (goal, definition) in graph {
            self.nodes
                .entry(goal)
                .and_modify(|node| {
                    // Newly available substitutes may prune pending source builds.
                    if node.state == NodeState::Pending
                        && definition.operation == Operation::Restore
                    {
                        node.definition.operation = Operation::Restore;
                    }
                })
                .or_insert(Node {
                    definition,
                    state: NodeState::Pending,
                    queued_at: now,
                });
        }
        if !self.requests.contains_key(&request.attempt) {
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or("request sequence exhausted")?;
            if request.activates {
                self.latest_activation
                    .insert(request.target.clone(), request.attempt.clone());
            }
            self.requests.insert(
                request.attempt.clone(),
                RequestRecord {
                    request,
                    sequence: self.sequence,
                    cancelled: false,
                    admitted: true,
                },
            );
        }
        Ok(())
    }

    pub fn cancel(&mut self, attempt: &str) -> Result<(), String> {
        self.requests
            .get_mut(attempt)
            .ok_or("unknown request")?
            .cancelled = true;
        Ok(())
    }

    /// Intake and graph preparation may precede the caller's live safety stages.
    pub fn hold(&mut self, attempt: &str) -> Result<(), String> {
        self.requests
            .get_mut(attempt)
            .ok_or("unknown request")?
            .admitted = false;
        Ok(())
    }

    pub fn admit(&mut self, attempt: &str) -> Result<(), String> {
        let record = self.requests.get_mut(attempt).ok_or("unknown request")?;
        if record.cancelled {
            return Err("cancelled request requires an explicit retry".into());
        }
        record.admitted = true;
        Ok(())
    }

    /// Retry is explicit; the frontend still reruns its own required live stages.
    pub fn retry(&mut self, attempt: &str) -> Result<(), String> {
        let roots = self
            .requests
            .get(attempt)
            .ok_or("unknown request")?
            .request
            .roots
            .clone();
        for goal in self.needed(&roots) {
            let node = self.nodes.get_mut(&goal).ok_or("missing retry goal")?;
            if matches!(node.state, NodeState::Failed(_)) {
                node.state = NodeState::Pending;
            }
        }
        let record = self.requests.get_mut(attempt).ok_or("unknown request")?;
        record.cancelled = false;
        record.admitted = false;
        Ok(())
    }

    /// Failure is propagated only through dependencies actually needed for building.
    pub fn outcome(&self, attempt: &str) -> Result<Outcome, String> {
        let record = self.requests.get(attempt).ok_or("unknown request")?;
        if record.cancelled {
            return Ok(Outcome::Cancelled);
        }
        let needed = self.needed(&record.request.roots);
        for goal in &needed {
            if let NodeState::Failed(detail) = &self.nodes[goal].state {
                return Ok(Outcome::Failed(format!("{goal}: {detail}")));
            }
        }
        if record
            .request
            .roots
            .iter()
            .all(|goal| self.nodes[goal].state == NodeState::Complete)
        {
            Ok(Outcome::Ready)
        } else {
            Ok(Outcome::Pending)
        }
    }

    fn needed(&self, roots: &BTreeSet<Goal>) -> BTreeSet<Goal> {
        let mut needed = BTreeSet::new();
        let mut todo: Vec<_> = roots.iter().cloned().collect();
        while let Some(goal) = todo.pop() {
            if !needed.insert(goal.clone()) {
                continue;
            }
            let node = &self.nodes[&goal];
            if node.state != NodeState::Complete && node.definition.operation == Operation::Build {
                todo.extend(node.definition.dependencies.iter().cloned());
            }
        }
        needed
    }

    /// Select from the ready frontier; running goals are never reprioritized.
    pub fn dispatch(&mut self, now: u64) -> Result<Option<Dispatch>, String> {
        if self.fence.is_some() {
            return Ok(None);
        }
        let mut interests: BTreeMap<Goal, usize> = BTreeMap::new();
        let mut roots = BTreeSet::new();
        // Native Nix may produce several named outputs in one builder process.
        // A second output of that derivation must not occupy another worker.
        let running_derivations: BTreeSet<_> = self
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.state, NodeState::Running { .. }))
            .map(|(goal, _)| goal.derivation.as_str())
            .collect();
        for (id, record) in &self.requests {
            if !record.admitted {
                continue;
            }
            if self.outcome(id)? != Outcome::Pending {
                continue;
            }
            roots.extend(record.request.roots.iter().cloned());
            for goal in self.needed(&record.request.roots) {
                *interests.entry(goal).or_default() += 1;
            }
        }
        let chosen = interests
            .iter()
            .filter(|(goal, _)| {
                let node = &self.nodes[*goal];
                node.state == NodeState::Pending
                    && !running_derivations.contains(goal.derivation.as_str())
                    && (node.definition.operation == Operation::Restore
                        || node
                            .definition
                            .dependencies
                            .iter()
                            .all(|dep| self.nodes[dep].state == NodeState::Complete))
            })
            .min_by_key(|(goal, count)| {
                let node = &self.nodes[*goal];
                let aged = now.saturating_sub(node.queued_at) >= self.aging_seconds;
                (
                    !aged,
                    if aged { node.queued_at } else { 0 },
                    if aged { 0 } else { usize::MAX - **count },
                    !roots.contains(*goal),
                    node.queued_at,
                    (*goal).clone(),
                )
            })
            .map(|(goal, _)| goal.clone());
        let Some(goal) = chosen else {
            return Ok(None);
        };
        self.dispatch_serial = self
            .dispatch_serial
            .checked_add(1)
            .ok_or("dispatch sequence exhausted")?;
        let node = self.nodes.get_mut(&goal).ok_or("missing dispatched node")?;
        node.state = NodeState::Running {
            serial: self.dispatch_serial,
        };
        Ok(Some(Dispatch {
            goal,
            definition: node.definition.clone(),
            serial: self.dispatch_serial,
        }))
    }

    pub fn finish(&mut self, dispatch: &Dispatch, result: Result<(), String>) {
        if let Some(node) = self.nodes.get_mut(&dispatch.goal) {
            if node.state
                == (NodeState::Running {
                    serial: dispatch.serial,
                })
            {
                node.state = result.map_or_else(NodeState::Failed, |()| NodeState::Complete);
            }
        }
    }

    /// Store evidence replaces interrupted receipts, including vanished complete outputs.
    /// The backend must retain GC roots before passing this evidence.
    pub fn reconcile(&mut self, valid: &BTreeSet<Goal>) {
        for (goal, node) in &mut self.nodes {
            if valid.contains(goal) {
                node.state = NodeState::Complete;
            } else if matches!(node.state, NodeState::Running { .. } | NodeState::Complete) {
                node.state = NodeState::Pending;
            }
        }
    }

    pub fn running(&self) -> usize {
        self.nodes
            .values()
            .filter(|node| matches!(node.state, NodeState::Running { .. }))
            .count()
    }

    pub fn drained(&self) -> bool {
        self.running() == 0
    }

    pub fn fence(&mut self, attempt: &str) -> Result<String, String> {
        if !self.requests.contains_key(attempt) {
            return Err("unknown request".into());
        }
        if let Some(fence) = &self.fence {
            return if fence.attempt == attempt {
                Ok(fence.token.clone())
            } else {
                Err(format!("builder fenced by {}", fence.attempt))
            };
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("fence sequence exhausted")?;
        let token = format!("{}-{}", attempt, self.sequence);
        self.fence = Some(Fence {
            token: token.clone(),
            attempt: attempt.into(),
        });
        Ok(token)
    }

    pub fn release_fence(&mut self, token: &str) -> Result<(), String> {
        if self.fence.as_ref().is_none_or(|fence| fence.token != token) {
            return Err("fence token mismatch".into());
        }
        if !self.drained() {
            return Err("builder workers have not drained".into());
        }
        self.fence = None;
        Ok(())
    }

    /// Newest submitted activating request wins. Build-only requests never supersede.
    /// Frontends call this while holding the existing target activation lease.
    pub fn authorize_activation(&self, attempt: &str) -> Result<(), String> {
        let record = self.requests.get(attempt).ok_or("unknown request")?;
        if !record.request.activates {
            return Err("build-only request cannot activate".into());
        }
        if !record.admitted {
            return Err("request has not passed caller admission".into());
        }
        if self
            .latest_activation
            .get(&record.request.target)
            .map(String::as_str)
            != Some(attempt)
        {
            return Err("superseded by a newer activation request for this target".into());
        }
        if self.outcome(attempt)? != Outcome::Ready {
            return Err("construction is not ready".into());
        }
        Ok(())
    }
}

fn validate_graph(roots: &BTreeSet<Goal>, graph: &Graph) -> Result<(), String> {
    if roots.iter().any(|root| !graph.contains_key(root)) {
        return Err("missing root definition".into());
    }
    let mut remaining: BTreeMap<_, _> = graph
        .iter()
        .map(|(goal, def)| (goal.clone(), def.dependencies.len()))
        .collect();
    let mut users: BTreeMap<Goal, Vec<Goal>> = BTreeMap::new();
    for (goal, definition) in graph {
        if definition.output_path.is_empty() {
            return Err(format!("missing output for {goal}"));
        }
        for dep in &definition.dependencies {
            if !graph.contains_key(dep) {
                return Err(format!("missing dependency {dep}"));
            }
            users.entry(dep.clone()).or_default().push(goal.clone());
        }
    }
    let mut ready: Vec<_> = remaining
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(g, _)| g.clone())
        .collect();
    let mut visited = 0;
    while let Some(goal) = ready.pop() {
        visited += 1;
        for user in users.get(&goal).into_iter().flatten() {
            let n = remaining.get_mut(user).ok_or("missing graph node")?;
            *n -= 1;
            if *n == 0 {
                ready.push(user.clone());
            }
        }
    }
    if visited != graph.len() {
        return Err("dependency cycle".into());
    }
    Ok(())
}
