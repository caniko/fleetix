//! Offline policy handover. The old fence is retained as immutable evidence;
//! no old-policy interest is allowed to run through the replacement coordinator.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    previous: Config,
    next: Config,
    train: Train,
    archives: Vec<Archive>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    previous: Config,
    next: Config,
    token: String,
    snapshot: String,
}

fn lease(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("rollover lease must be an euid-owned private regular file".into());
    }
    file.try_lock_exclusive()
        .map_err(|e| format!("stop the old coordinator before rollover: {e}"))?;
    Ok(file)
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|e| e.to_string())
}

fn read_snapshot(history: &Path, marker: &Marker) -> Result<Snapshot, String> {
    if marker.snapshot.len() != 64 || !marker.snapshot.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid rollover snapshot identity".into());
    }
    let raw = fs::read(history.join(&marker.snapshot).join("snapshot.json"))
        .map_err(|e| e.to_string())?;
    if format!("{:x}", Sha256::digest(&raw)) != marker.snapshot {
        return Err("rollover snapshot checksum changed".into());
    }
    let snapshot: Snapshot = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if snapshot.version != VERSION
        || snapshot.train.version != VERSION
        || snapshot.previous != marker.previous
        || snapshot.next != marker.next
        || snapshot.train.policy != marker.previous.policy
        || snapshot.train.aging_seconds != marker.previous.aging_seconds
        || snapshot.train.running() != 0
        || snapshot
            .train
            .fence
            .as_ref()
            .is_none_or(|f| f.token != marker.token)
        || active_requests(&snapshot.train)? != 0
    {
        return Err("rollover snapshot contract changed".into());
    }
    Ok(snapshot)
}

/// Archive a stopped, drained, fenced train and initialize a different policy.
///
/// Every request must already be terminal; cancel pending requests explicitly
/// through the original coordinator. This operation takes the coordinator and
/// socket leases, preserves the entire old journal (including supersession and
/// fence evidence), archives requests before releasing their roots, and publishes
/// the new empty journal last. An interrupted handover blocks service startup and
/// is completed only by repeating the exact configs and fence token. Historical
/// status/retirement stays available, but old requests cannot execute or activate.
pub fn rollover(
    previous: &Config,
    next: &Config,
    backend: &dyn Backend,
    token: &str,
) -> Result<PathBuf, String> {
    if previous.state_dir != next.state_dir
        || previous.socket != next.socket
        || previous.policy == next.policy
        || previous.policy.is_empty()
        || next.policy.is_empty()
        || next.policy.len() > wire::FIELD_LIMIT
        || next.workers == 0
        || next.workers > 64
        || next.queue_limit == 0
        || !(1..=16).contains(&next.planning_workers)
        || !(1..=86_400).contains(&next.planning_timeout_seconds)
    {
        return Err(
            "rollover requires unchanged state/socket locations and a valid different policy"
                .into(),
        );
    }
    private_dir(&previous.state_dir)?;
    private_dir(&previous.state_dir.join("archive"))?;
    private_dir(previous.socket.parent().ok_or("socket has no parent")?)?;
    let _coordinator = lease(&previous.state_dir.join("coordinator.lock"))?;
    let _socket = lease(&previous.socket.with_extension("lock"))?;
    let marker_path = previous.state_dir.join("rollover.json");
    let current: Train = serde_json::from_reader(
        File::open(previous.state_dir.join("train.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if current.version != VERSION {
        return Err("unsupported rollover journal version; evidence retained".into());
    }
    let marker: Option<Marker> = match File::open(&marker_path) {
        Ok(file) => Some(serde_json::from_reader(file).map_err(|e| e.to_string())?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let history = previous.state_dir.join("rollovers");
    private_dir(&history)?;
    // A completed retry does not reset a replacement train which has since run.
    if marker.is_none() && current.policy == next.policy {
        for entry in fs::read_dir(&history).map_err(|e| e.to_string())? {
            let path = entry
                .map_err(|e| e.to_string())?
                .path()
                .join("receipt.json");
            if let Ok(file) = File::open(&path) {
                let receipt: Marker = serde_json::from_reader(file).map_err(|e| e.to_string())?;
                if receipt.previous == *previous && receipt.next == *next && receipt.token == token
                {
                    read_snapshot(&history, &receipt)?;
                    if path != history.join(&receipt.snapshot).join("receipt.json") {
                        return Err("rollover receipt is outside its snapshot namespace".into());
                    }
                    return Ok(path);
                }
            }
        }
        return Err("new policy has no matching completed rollover receipt".into());
    }
    let (marker, snapshot) = if let Some(marker) = marker {
        if marker.previous != *previous
            || marker.next != *next
            || marker.token != token
            || marker.snapshot.len() != 64
            || !marker.snapshot.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(
                "interrupted rollover has different configs, fence or snapshot identity".into(),
            );
        }
        let snapshot = read_snapshot(&history, &marker)?;
        (marker, snapshot)
    } else {
        if current.version != VERSION
            || current.policy != previous.policy
            || current.aging_seconds != previous.aging_seconds
        {
            return Err("old journal does not match the original coordinator contract".into());
        }
        let fence = current
            .fence
            .as_ref()
            .ok_or("rollover requires the retained activation fence")?;
        if fence.token != token || current.running() != 0 {
            return Err("rollover fence changed or builders have not drained".into());
        }
        if active_requests(&current)? != 0 {
            return Err("cancel pending requests explicitly before policy rollover".into());
        }
        let mut archives = Vec::new();
        let mut entries = fs::read_dir(previous.state_dir.join("archive"))
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for path in entries {
            let archive: Archive =
                serde_json::from_reader(File::open(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            if archive.version != VERSION
                || archive.policy.is_empty()
                || path != archive_path(previous, &archive.record.request.attempt)
            {
                return Err("invalid retained retirement archive".into());
            }
            archives.push(archive);
        }
        let snapshot = Snapshot {
            version: VERSION,
            previous: previous.clone(),
            next: next.clone(),
            train: current.clone(),
            archives,
        };
        let raw = json(&snapshot)?;
        let digest = format!("{:x}", Sha256::digest(&raw));
        let directory = history.join(&digest);
        private_dir(&directory)?;
        crate::fsutil::atomic_write(&directory.join("snapshot.json"), &raw)
            .map_err(|e| e.to_string())?;
        // atomic_write syncs the snapshot directory, but its entry in the
        // history parent must also survive before the interruption marker does.
        File::open(&history)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
        let marker = Marker {
            previous: previous.clone(),
            next: next.clone(),
            token: token.into(),
            snapshot: digest,
        };
        crate::fsutil::atomic_write(&marker_path, &json(&marker)?).map_err(|e| e.to_string())?;
        (marker, snapshot)
    };
    let replacement = Train::new(next.policy.clone(), next.aging_seconds);
    if snapshot.version != VERSION
        || snapshot.previous != *previous
        || snapshot.next != *next
        || snapshot.train.policy != previous.policy
        || snapshot.train.running() != 0
        || snapshot
            .train
            .fence
            .as_ref()
            .is_none_or(|f| f.token != token)
        || active_requests(&snapshot.train)? != 0
        || (json(&current)? != json(&snapshot.train)? && json(&current)? != json(&replacement)?)
    {
        return Err("rollover journal or snapshot changed; evidence retained".into());
    }
    let directory = history.join(&marker.snapshot);
    // Preserve existing tombstones and interrupted retirement root-release work.
    for original in &snapshot.archives {
        if !original.roots_released {
            let mut archive = read_archive(previous, &original.record.request.attempt)?
                .ok_or("missing original retirement archive")?;
            if !archive.roots_released {
                backend.release(&archive.record.request, &archive.graph)?;
                archive.roots_released = true;
                save_archive(previous, &archive)?;
            }
        }
    }
    for (attempt, record) in &snapshot.train.requests {
        let outcome = snapshot.train.outcome(attempt)?;
        let graph = snapshot.train.retained_graph(attempt)?;
        let mut archive = Archive {
            version: VERSION,
            policy: previous.policy.clone(),
            record: record.clone(),
            outcome,
            graph,
            roots_released: false,
        };
        if let Some(existing) = read_archive(previous, attempt)? {
            let released = existing.roots_released;
            archive.roots_released = released;
            if json(&archive)? != json(&existing)? {
                return Err("retirement archive differs from rollover evidence".into());
            }
        }
        save_archive(previous, &archive)?;
        if !archive.roots_released {
            backend.release(&archive.record.request, &archive.graph)?;
            archive.roots_released = true;
            save_archive(previous, &archive)?;
        }
    }
    save(next, &replacement)?;
    let receipt = directory.join("receipt.json");
    crate::fsutil::atomic_write(&receipt, &json(&marker)?).map_err(|e| e.to_string())?;
    fs::remove_file(&marker_path).map_err(|e| e.to_string())?;
    File::open(&previous.state_dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(receipt)
}
