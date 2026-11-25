//! Generic keyed request superseding and last-writer-wins coalescing.
//!
//! Serializing stacked updates to the same key (for example, 7 successive
//! commits for the same flake input or repeated switches for the same host)
//! causes queue backlogs that starve operators and waste builder capacity on
//! already-obsolete revisions.
//!
//! [`coalesce_keyed`] merges compatible queued items into one effective batch
//! using last-writer-wins per key while:
//! - preserving causal order across incompatible modes (an item never overtakes
//!   a deferred item that touches the same key),
//! - preserving non-overridden keys on multi-key items (upgrading only the
//!   overwritten key while keeping their remaining keys intact),
//! - identifying items whose keys were completely superseded so they need not
//!   run serially or on solo fallback, and
//! - bounding deferrals (`max_deferrals`) so a hot key cannot starve forever.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One pending request carrying a compatibility mode and a set of keyed specs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyedItem<K: Ord, V> {
    pub id: String,
    /// Compatibility bucket (e.g. `"edit"` vs `"refresh"`, `"switch"` vs `"boot"`).
    pub mode: String,
    /// Keys touched by this item and the target specification for each key.
    pub keys: BTreeMap<K, V>,
    /// Number of prior trains that deferred/superseded this item already.
    #[serde(default)]
    pub deferrals: u32,
}

/// Controls how conflicting values for the same key within one mode behave.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoalescePolicy {
    /// When `true`, a newer compatible item overwrites an older item's spec for
    /// the same key in the merged batch instead of forcing serial batches.
    pub allow_last_writer_wins: bool,
    /// Anti-starvation cap: once an older item has been deferred this many
    /// times, newer writes to its keys wait for the next batch instead of
    /// superseding it again (`0` means no deferral cap).
    pub max_deferrals: u32,
}

impl Default for CoalescePolicy {
    fn default() -> Self {
        Self {
            allow_last_writer_wins: true,
            max_deferrals: 2,
        }
    }
}

/// An older item whose keys were all overwritten with newer specifications.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupersededItem {
    pub id: String,
    /// Ordered IDs of the newer boarded item(s) that overwrote its keys.
    pub by: Vec<String>,
}

/// Deduplicated fallback group for solo reruns if the merged batch fails.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SoloGroup<K: Ord, V> {
    pub ids: Vec<String>,
    pub keys: BTreeMap<K, V>,
}

/// Batch plan computed by [`coalesce_keyed`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoalescePlan<K: Ord, V> {
    /// Mode selected for this batch (from the oldest item), or `None` if empty.
    pub mode: Option<String>,
    /// Non-superseded item IDs riding this batch in arrival order.
    pub boarded_ids: Vec<String>,
    /// Older items whose keys were completely overwritten by newer boarded items.
    pub superseded: Vec<SupersededItem>,
    /// Items left queued for a subsequent batch (incompatible mode, blocked key,
    /// or deferral cap reached).
    pub deferred_ids: Vec<String>,
    /// Merged key→spec map to execute in a single run.
    pub effective_specs: BTreeMap<K, V>,
    /// Minimal deduplicated solo-rerun groups (excluding strictly superseded
    /// obsolete items) for isolation if the merged run fails.
    pub solo_groups: Vec<SoloGroup<K, V>>,
}

/// Compute a coalesced batch from `items` (ordered oldest-first).
pub fn coalesce_keyed<K, V>(items: &[KeyedItem<K, V>], policy: CoalescePolicy) -> CoalescePlan<K, V>
where
    K: Ord + Clone,
    V: PartialEq + Eq + Clone,
{
    let Some(first) = items.first() else {
        return CoalescePlan {
            mode: None,
            boarded_ids: Vec::new(),
            superseded: Vec::new(),
            deferred_ids: Vec::new(),
            effective_specs: BTreeMap::new(),
            solo_groups: Vec::new(),
        };
    };
    let batch_mode = first.mode.clone();
    let mut accepted: Vec<&KeyedItem<K, V>> = Vec::new();
    let mut deferred_ids = Vec::new();
    let mut blocked_keys: BTreeSet<K> = BTreeSet::new();
    let mut effective_specs: BTreeMap<K, V> = BTreeMap::new();
    // Keys owned by an accepted item that already hit `max_deferrals` cannot be
    // overwritten in this batch.
    let mut pinned_keys: BTreeSet<K> = BTreeSet::new();

    for item in items {
        let touches_blocked = item.keys.keys().any(|key| blocked_keys.contains(key));
        if item.mode != batch_mode || touches_blocked {
            blocked_keys.extend(item.keys.keys().cloned());
            deferred_ids.push(item.id.clone());
            continue;
        }
        let has_conflicting_spec = item
            .keys
            .iter()
            .any(|(key, value)| effective_specs.get(key).is_some_and(|prior| prior != value));
        let hits_pinned = has_conflicting_spec
            && item.keys.iter().any(|(key, value)| {
                pinned_keys.contains(key)
                    && effective_specs.get(key).is_some_and(|prior| prior != value)
            });
        if has_conflicting_spec && (!policy.allow_last_writer_wins || hits_pinned) {
            blocked_keys.extend(item.keys.keys().cloned());
            deferred_ids.push(item.id.clone());
            continue;
        }
        if policy.max_deferrals > 0 && item.deferrals >= policy.max_deferrals {
            pinned_keys.extend(item.keys.keys().cloned());
        }
        for (key, value) in &item.keys {
            effective_specs.insert(key.clone(), value.clone());
        }
        accepted.push(item);
    }

    // Track which accepted item wrote the final effective value for each key.
    // When multiple items write the exact same final value, none supersedes the
    // other; only an earlier item whose value differs from the final spec on
    // EVERY key it requested is strictly superseded.
    let mut final_writer: BTreeMap<K, String> = BTreeMap::new();
    for item in &accepted {
        for (key, value) in &item.keys {
            if effective_specs.get(key) == Some(value) {
                final_writer.insert(key.clone(), item.id.clone());
            }
        }
    }

    let mut boarded: Vec<&KeyedItem<K, V>> = Vec::new();
    let mut superseded = Vec::new();
    for item in &accepted {
        let all_overwritten = !item.keys.is_empty()
            && item.keys.iter().all(|(key, value)| {
                effective_specs
                    .get(key)
                    .is_some_and(|final_v| final_v != value)
            });
        if all_overwritten {
            let mut by = Vec::new();
            let mut seen = BTreeSet::new();
            for key in item.keys.keys() {
                if let Some(writer) = final_writer.get(key) {
                    if seen.insert(writer.as_str()) {
                        by.push(writer.clone());
                    }
                }
            }
            superseded.push(SupersededItem {
                id: item.id.clone(),
                by,
            });
        } else {
            boarded.push(item);
        }
    }

    // Build deduplicated solo fallback groups from non-superseded boarded items.
    let mut solo_groups: Vec<SoloGroup<K, V>> = Vec::new();
    for item in &boarded {
        if let Some(existing) = solo_groups.iter_mut().find(|group| group.keys == item.keys) {
            existing.ids.push(item.id.clone());
        } else {
            solo_groups.push(SoloGroup {
                ids: vec![item.id.clone()],
                keys: item.keys.clone(),
            });
        }
    }

    CoalescePlan {
        mode: Some(batch_mode),
        boarded_ids: boarded.iter().map(|item| item.id.clone()).collect(),
        superseded,
        deferred_ids,
        effective_specs,
        solo_groups,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, mode: &str, pairs: &[(&str, &str)]) -> KeyedItem<String, String> {
        KeyedItem {
            id: id.into(),
            mode: mode.into(),
            keys: pairs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            deferrals: 0,
        }
    }

    #[test]
    fn collapses_stacked_updates_while_preserving_sibling_keys_in_multi_key_items() {
        let items = vec![
            item("pr44", "edit", &[("infernix", "rev44")]),
            item("pr45", "edit", &[("infernix", "rev45")]),
            item("pr46", "edit", &[("infernix", "rev46")]),
            item(
                "pr49",
                "edit",
                &[
                    ("ai-skills", "revA"),
                    ("skillnet", "revS"),
                    ("infernix", "rev49"),
                ],
            ),
            item("pr50", "edit", &[("infernix", "rev50")]),
        ];
        let plan = coalesce_keyed(&items, CoalescePolicy::default());
        assert_eq!(plan.mode.as_deref(), Some("edit"));
        assert_eq!(plan.boarded_ids, vec!["pr49", "pr50"]);
        assert_eq!(
            plan.superseded,
            vec![
                SupersededItem {
                    id: "pr44".into(),
                    by: vec!["pr50".into()],
                },
                SupersededItem {
                    id: "pr45".into(),
                    by: vec!["pr50".into()],
                },
                SupersededItem {
                    id: "pr46".into(),
                    by: vec!["pr50".into()],
                },
            ]
        );
        assert!(plan.deferred_ids.is_empty());
        assert_eq!(
            plan.effective_specs,
            BTreeMap::from([
                ("ai-skills".into(), "revA".into()),
                ("infernix".into(), "rev50".into()),
                ("skillnet".into(), "revS".into()),
            ])
        );
        // On merged failure, only the two non-superseded items run solo, each
        // with its own coherent spec; obsolete pr44..pr46 never re-run.
        assert_eq!(plan.solo_groups.len(), 2);
        assert_eq!(plan.solo_groups[0].ids, vec!["pr49"]);
        assert_eq!(plan.solo_groups[1].ids, vec!["pr50"]);
    }

    #[test]
    fn preserves_causal_order_across_modes_and_respects_starvation_cap() {
        let items = vec![
            item("e1", "edit", &[("a", "v1")]),
            item("r1", "refresh", &[("a", "latest")]),
            item("e2", "edit", &[("a", "v2")]),
            item("e3", "edit", &[("b", "v1")]),
        ];
        let plan = coalesce_keyed(&items, CoalescePolicy::default());
        // e2 cannot overtake r1 on key "a", while unrelated key "b" (e3) joins.
        assert_eq!(plan.boarded_ids, vec!["e1", "e3"]);
        assert_eq!(plan.deferred_ids, vec!["r1", "e2"]);

        // Starvation cap prevents an already-deferred item from being overwritten again.
        let mut aged = item("old", "edit", &[("a", "v1")]);
        aged.deferrals = 2;
        let newer = item("new", "edit", &[("a", "v2")]);
        let capped = coalesce_keyed(
            &[aged, newer],
            CoalescePolicy {
                allow_last_writer_wins: true,
                max_deferrals: 2,
            },
        );
        assert_eq!(capped.boarded_ids, vec!["old"]);
        assert_eq!(capped.deferred_ids, vec!["new"]);
        assert!(capped.superseded.is_empty());
    }

    #[test]
    fn identical_requests_share_boarded_and_solo_group_without_superseding() {
        let items = vec![
            item("a", "edit", &[("x", "v1")]),
            item("b", "edit", &[("x", "v1")]),
        ];
        let plan = coalesce_keyed(&items, CoalescePolicy::default());
        assert_eq!(plan.boarded_ids, vec!["a", "b"]);
        assert!(plan.superseded.is_empty());
        assert_eq!(plan.solo_groups.len(), 1);
        assert_eq!(plan.solo_groups[0].ids, vec!["a", "b"]);
    }
}
