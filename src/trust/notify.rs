// Desktop prompting via `notify-send --wait --action`.
//
// Returns the action chosen by the user; `None` means dismissed or no
// notification daemon/actions available. Callers fall back to logging the
// pending `fleetix trust integrate <id>` command when prompting is not
// possible.

use super::openssh::Entry;
use miette::{miette, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Integrate,
    Ignore,
}

/// Prompt for one proposal. Errors (missing notify-send, dead daemon) are
/// returned so the caller can decide the fallback.
pub fn prompt(entry: &Entry) -> Result<Option<Action>> {
    let hostnames = entry.host_names.join(", ");
    let summary = format!("Fleetix: new SSH host key for {hostnames}");
    let mut body = format!("{} ({})", entry.key_text, entry.key_type);
    if let Some(fingerprint) = &entry.fingerprint {
        body.push_str(&format!("\nFingerprint: {fingerprint}"));
    }
    if let Some(comment) = &entry.comment {
        body.push_str(&format!("\nComment: {comment}"));
    }
    body.push_str("\n\nIntegrate to declare it in Trust.pkl, or ignore.");

    let output = std::process::Command::new("notify-send")
        .args([
            "--app-name=Fleetix",
            "--urgency=normal",
            "--wait",
            "--action=integrate=Integrate",
            "--action=ignore=Ignore",
        ])
        .arg(&summary)
        .arg(&body)
        .output()
        .map_err(|error| miette!("notify-send failed: {error}"))?;
    if !output.status.success() {
        return Err(miette!("notify-send exited with {}", output.status));
    }
    let chosen = String::from_utf8_lossy(&output.stdout);
    Ok(action_from_stdout(&chosen))
}

/// Map notify-send's printed action key to an [`Action`]. Dismissal, timeout,
/// and daemon-side closing all print nothing (or a non-action key).
fn action_from_stdout(stdout: &str) -> Option<Action> {
    match stdout.trim() {
        "integrate" => Some(Action::Integrate),
        "ignore" => Some(Action::Ignore),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_mapping_covers_dismiss() {
        assert_eq!(action_from_stdout("integrate"), Some(Action::Integrate));
        assert_eq!(action_from_stdout("ignore\n"), Some(Action::Ignore));
        assert_eq!(action_from_stdout(""), None);
        assert_eq!(action_from_stdout("timeout"), None);
    }
}
