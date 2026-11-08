//! Live, read-only memory views of the host running the server: `/proc`,
//! cgroup v2 and the kernel journal. Unreadable sources are omitted rather
//! than reported as zero, so an absent field never reads as a measurement.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

const MEMINFO_KEYS: &[&str] = &[
    "MemTotal",
    "MemFree",
    "MemAvailable",
    "AnonPages",
    "Cached",
    "Buffers",
    "Shmem",
    "Slab",
    "SReclaimable",
    "SUnreclaim",
    "PageTables",
    "KernelStack",
    "SwapTotal",
    "SwapFree",
    "Zswap",
    "Zswapped",
    "Committed_AS",
    "CommitLimit",
];

fn read(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// `/proc/meminfo` subset in bytes.
pub(super) fn parse_meminfo(text: &str) -> Map<String, Value> {
    text.lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
            MEMINFO_KEYS
                .contains(&key)
                .then(|| (key.to_owned(), json!(kib * 1024)))
        })
        .collect()
}

/// `/proc/pressure/*`: `{"some": {"avg10": ..}, "full": {..}}`.
pub(super) fn parse_psi(text: &str) -> Map<String, Value> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let kind = fields.next()?;
            let values: Map<String, Value> = fields
                .filter_map(|field| {
                    let (key, value) = field.split_once('=')?;
                    Some((key.to_owned(), json!(value.parse::<f64>().ok()?)))
                })
                .collect();
            Some((kind.to_owned(), Value::Object(values)))
        })
        .collect()
}

/// `/proc/swaps` rows with sizes in bytes.
pub(super) fn parse_swaps(text: &str) -> Vec<Value> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [device, kind, size, used, priority] = fields[..] else {
                return None;
            };
            Some(json!({
                "device": device,
                "type": kind,
                "sizeBytes": size.parse::<u64>().ok()? * 1024,
                "usedBytes": used.parse::<u64>().ok()? * 1024,
                "priority": priority.parse::<i64>().ok()?,
            }))
        })
        .collect()
}

pub(super) fn metrics(proc_root: &Path, sys_root: &Path) -> Value {
    let mut out = Map::new();
    if let Some(text) = read(proc_root.join("meminfo")) {
        out.insert("memory".into(), Value::Object(parse_meminfo(&text)));
    }
    if let Some(text) = read(proc_root.join("swaps")) {
        out.insert("swap".into(), Value::Array(parse_swaps(&text)));
    }
    let pressure: Map<String, Value> = ["memory", "cpu", "io"]
        .into_iter()
        .filter_map(|kind| {
            let text = read(proc_root.join("pressure").join(kind))?;
            Some((kind.to_owned(), Value::Object(parse_psi(&text))))
        })
        .collect();
    if !pressure.is_empty() {
        out.insert("pressure".into(), Value::Object(pressure));
    }
    if let Some(text) = read(proc_root.join("loadavg")) {
        let load: Vec<f64> = text
            .split_whitespace()
            .take(3)
            .filter_map(|value| value.parse().ok())
            .collect();
        out.insert("loadAverage".into(), json!(load));
    }
    if let Some(seconds) = read(proc_root.join("uptime"))
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
    {
        out.insert("uptimeSeconds".into(), json!(seconds));
    }
    if let Ok(cpus) = std::thread::available_parallelism() {
        out.insert("cpus".into(), json!(cpus.get()));
    }
    let gpus: Vec<Value> = fs::read_dir(sys_root.join("class/drm"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            // renderD* nodes share card*'s device; connectors (card1-DP-1) have none.
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card") || name.contains('-') {
                return None;
            }
            let device = entry.path().join("device");
            let number = |name: &str| read(device.join(name))?.trim().parse::<u64>().ok();
            Some(json!({
                "card": name,
                "vramUsedBytes": number("mem_info_vram_used")?,
                "vramTotalBytes": number("mem_info_vram_total")?,
            }))
        })
        .collect();
    if !gpus.is_empty() {
        out.insert("gpus".into(), Value::Array(gpus));
    }
    Value::Object(out)
}

fn key_values(text: &str) -> BTreeMap<&str, u64> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key, value.trim().parse().ok()?))
        })
        .collect()
}

/// `None` when unreadable, `Some(null)` for an explicit unlimited `max`.
fn bytes_or_max(text: Option<String>) -> Option<Value> {
    match text?.trim() {
        "max" => Some(Value::Null),
        value => value.parse::<u64>().ok().map(|n| json!(n)),
    }
}

/// cgroup v2 memory accounting at most `depth` levels below `root`, largest
/// first, keeping cgroups whose `memory.current` is at least `min_bytes`.
pub(super) fn cgroups(root: &Path, depth: usize, min_bytes: u64, limit: usize) -> Value {
    fn walk(root: &Path, dir: &Path, depth: usize, min_bytes: u64, out: &mut Vec<(u64, Value)>) {
        let current =
            read(dir.join("memory.current")).and_then(|text| text.trim().parse::<u64>().ok());
        if let Some(current) = current.filter(|current| *current >= min_bytes) {
            let path = dir.strip_prefix(root).unwrap_or(dir);
            let mut entry = Map::new();
            entry.insert("path".into(), json!(format!("/{}", path.display())));
            entry.insert("currentBytes".into(), json!(current));
            if let Some(stat) = read(dir.join("memory.stat")) {
                let stat = key_values(&stat);
                for (key, name) in [("anonBytes", "anon"), ("fileBytes", "file")] {
                    if let Some(value) = stat.get(name) {
                        entry.insert(key.into(), json!(value));
                    }
                }
            }
            for (key, file) in [
                ("peakBytes", "memory.peak"),
                ("maxBytes", "memory.max"),
                ("highBytes", "memory.high"),
                ("swapBytes", "memory.swap.current"),
            ] {
                if let Some(value) = bytes_or_max(read(dir.join(file))) {
                    entry.insert(key.into(), value);
                }
            }
            if let Some(events) = read(dir.join("memory.events")) {
                entry.insert("events".into(), json!(key_values(&events)));
            }
            out.push((current, Value::Object(entry)));
        }
        if depth == 0 {
            return;
        }
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                walk(root, &entry.path(), depth - 1, min_bytes, out);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, root, depth, min_bytes, &mut found);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let total = found.len();
    let cgroups: Vec<Value> = found.into_iter().take(limit).map(|(_, v)| v).collect();
    json!({"cgroups": cgroups, "matched": total})
}

/// Kernel OOM kills from `journalctl -o short-iso` kernel text: one event per
/// `oom-kill:` line, with the processes killed for it counted by command.
pub(super) fn parse_oom_events(text: &str) -> Vec<Value> {
    let mut events: Vec<(Map<String, Value>, BTreeMap<String, u64>)> = Vec::new();
    for line in text.lines() {
        if let Some(detail) = line.split_once("oom-kill:").map(|(_, rest)| rest) {
            let mut event: Map<String, Value> = detail
                .split(',')
                .filter_map(|field| field.split_once('='))
                .filter(|(key, _)| {
                    matches!(
                        *key,
                        "constraint" | "oom_memcg" | "task_memcg" | "task" | "pid" | "uid"
                    )
                })
                .map(|(key, value)| (key.to_owned(), json!(value.trim())))
                .collect();
            if let Some(time) = line.split_whitespace().next() {
                event.insert("time".into(), json!(time));
            }
            events.push((event, BTreeMap::new()));
        } else if let Some((_, rest)) = line.split_once("Killed process ") {
            let command = rest.split_once('(').and_then(|(_, rest)| rest.split_once(')'));
            if let (Some((command, _)), Some((_, killed))) = (command, events.last_mut()) {
                *killed.entry(command.to_owned()).or_default() += 1;
            }
        }
    }
    events
        .into_iter()
        .map(|(mut event, killed)| {
            event.insert("killedProcesses".into(), json!(killed.values().sum::<u64>()));
            event.insert("killedByCommand".into(), json!(killed));
            Value::Object(event)
        })
        .collect()
}

pub(super) fn oom_events(since: &str) -> Result<Value, String> {
    let output = std::process::Command::new("journalctl")
        // `-k` would also restrict to the current boot; `since` may span reboots.
        .args(["_TRANSPORT=kernel", "--no-pager", "-q", "-o", "short-iso"])
        // The `=` form keeps a caller value from being parsed as an option.
        .arg(format!("--since={since}"))
        .output()
        .map_err(|error| format!("could not run journalctl: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let events = parse_oom_events(&String::from_utf8_lossy(&output.stdout));
    Ok(json!({"since": since, "count": events.len(), "events": events}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_parsers_report_bytes_and_skip_unknown_rows() {
        let memory = parse_meminfo("MemTotal:  4 kB\nHugePages_Total: 0\nMemAvailable: 2 kB\n");
        assert_eq!(memory["MemTotal"], 4096);
        assert_eq!(memory["MemAvailable"], 2048);
        assert!(!memory.contains_key("HugePages_Total"));

        let psi = parse_psi(
            "some avg10=0.96 avg60=1.00 avg300=0.80 total=1\nfull avg10=0.85 avg60=0 avg300=0 total=2\n",
        );
        assert_eq!(psi["some"]["avg10"], 0.96);
        assert_eq!(psi["full"]["total"], 2.0);

        let swaps = parse_swaps(
            "Filename Type Size Used Priority\n/dev/nvme3n1p2 partition 2 1 100\n",
        );
        assert_eq!(swaps[0]["sizeBytes"], 2048);
        assert_eq!(swaps[0]["priority"], 100);
    }

    #[test]
    fn cgroup_walk_filters_sorts_and_maps_unlimited_to_null() {
        let root = tempfile::tempdir().unwrap();
        let write = |dir: &str, name: &str, text: &str| {
            let dir = root.path().join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(name), text).unwrap();
        };
        write("big.slice", "memory.current", "300\n");
        write("big.slice", "memory.max", "max\n");
        write("big.slice", "memory.stat", "anon 200\nfile 100\n");
        write("big.slice", "memory.events", "low 0\noom 3\noom_kill 76\n");
        write("big.slice/builders", "memory.current", "250\n");
        write("big.slice/builders", "memory.max", "17179869184\n");
        write("small.slice", "memory.current", "5\n");

        let found = cgroups(root.path(), 3, 10, 10);
        assert_eq!(found["matched"], 2);
        assert_eq!(found["cgroups"][0]["path"], "/big.slice");
        assert_eq!(found["cgroups"][0]["maxBytes"], Value::Null);
        assert_eq!(found["cgroups"][0]["anonBytes"], 200);
        assert_eq!(found["cgroups"][0]["events"]["oom_kill"], 76);
        assert_eq!(found["cgroups"][1]["maxBytes"], 17179869184u64);
        // Unreadable sources are absent, never confused with an unlimited `max`.
        let builders = found["cgroups"][1].as_object().unwrap();
        for absent in ["peakBytes", "anonBytes", "events", "swapBytes"] {
            assert!(!builders.contains_key(absent), "{absent}");
        }
        assert!(found["cgroups"][0].as_object().unwrap().contains_key("maxBytes"));
        assert_eq!(cgroups(root.path(), 0, 10, 10)["matched"], 0);
        assert_eq!(cgroups(root.path(), 3, 10, 1)["cgroups"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn oom_events_attribute_group_kills_to_their_event() {
        let log = "\
2026-11-08T18:19:56+0100 atlas kernel: g++ invoked oom-killer: gfp_mask=0xcc0
2026-11-08T18:19:56+0100 atlas kernel: oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=nix-daemon.service,mems_allowed=0,oom_memcg=/canix.slice/nix-builders,task_memcg=/canix.slice/nix-builders/uid-30015,task=cc1plus,pid=31000,uid=30015
2026-11-08T18:19:56+0100 atlas kernel: Memory cgroup out of memory: Killed process 31000 (cc1plus) total-vm:1kB
2026-11-08T18:19:56+0100 atlas kernel: Killed process 30790 (g++) total-vm:1kB
2026-11-08T18:19:56+0100 atlas kernel: Tasks in /canix.slice/nix-builders/uid-30015 are going to be killed due to memory.oom.group set
2026-11-08T18:19:56+0100 atlas kernel: Killed process 31001 (cc1plus) total-vm:1kB
";
        let events = parse_oom_events(log);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["oom_memcg"], "/canix.slice/nix-builders");
        assert_eq!(events[0]["uid"], "30015");
        assert_eq!(events[0]["time"], "2026-11-08T18:19:56+0100");
        assert_eq!(events[0]["killedProcesses"], 3);
        assert_eq!(events[0]["killedByCommand"]["cc1plus"], 2);
        assert!(parse_oom_events("Killed process 1 (x)\n").is_empty());
    }
}
