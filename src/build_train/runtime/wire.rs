//! Bound serialization as well as socket delivery. Full failures stay in the journal.
use super::{MAX_FRAME, Reply};
use crate::build_train::{Fence, Goal, Graph, Outcome, Request};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{self, Write};

pub(super) const FIELD_LIMIT: usize = 4096;
// Reserve space for the policy, bounded identities, failure detail and fence.
const OUTPUT_LIMIT: usize = MAX_FRAME as usize - 131_072;

struct LimitedJson {
    raw: Vec<u8>,
    limit: usize,
}

impl Write for LimitedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.raw.len()) {
            return Err(io::Error::other("protocol serialization limit exceeded"));
        }
        self.raw.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, String> {
    let mut writer = LimitedJson {
        raw: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|error| error.to_string())?;
    Ok(writer.raw)
}

pub(super) fn validate_outputs(request: &Request, graph: &Graph) -> Result<(), String> {
    let outputs: BTreeMap<_, _> = request
        .roots
        .iter()
        .filter_map(|goal| {
            graph
                .get(goal)
                .map(|definition| (goal, &definition.output_path))
        })
        .collect();
    serialize(&outputs, OUTPUT_LIMIT)
        .map(|_| ())
        .map_err(|_| "root output evidence exceeds protocol limit".into())
}

fn detail(raw: &str) -> String {
    if raw.len() <= FIELD_LIMIT {
        return raw.to_owned();
    }
    let mut end = FIELD_LIMIT;
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} [truncated; full detail retained in journal]",
        &raw[..end]
    )
}

#[derive(Serialize)]
struct ReplyFrame<'a> {
    version: u32,
    policy: &'a str,
    outcome: Option<Outcome>,
    fence: Option<&'a Fence>,
    running: usize,
    error: Option<String>,
    outputs: &'a BTreeMap<Goal, String>,
}

pub(super) fn encode(result: &Reply) -> Result<Vec<u8>, String> {
    let frame = ReplyFrame {
        version: result.version,
        policy: &result.policy,
        outcome: result.outcome.as_ref().map(|outcome| match outcome {
            Outcome::Failed(error) => Outcome::Failed(detail(error)),
            other => other.clone(),
        }),
        fence: result.fence.as_ref(),
        running: result.running,
        error: result.error.as_deref().map(detail),
        outputs: &result.outputs,
    };
    let mut raw = match serialize(&frame, MAX_FRAME as usize - 1) {
        Ok(raw) => raw,
        Err(_) => {
            // Older retained evidence may exceed today's admission budget. Do not
            // kill the coordinator or silently drop an already-applied command.
            let outputs = BTreeMap::new();
            let fallback = ReplyFrame {
                outputs: &outputs,
                error: Some("coordinator reply exceeds protocol limit; durable command may already be recorded; inspect request status before retrying".into()),
                ..frame
            };
            serialize(&fallback, MAX_FRAME as usize - 1)?
        }
    };
    raw.push(b'\n');
    Ok(raw)
}
