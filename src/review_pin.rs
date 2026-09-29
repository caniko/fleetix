//! Fail-closed evidence checks for consuming a reviewed, immutable Nix package.
//!
//! Evaluation and cache queries belong to the consumer CLI. This module checks
//! their results against the review report without running a build or changing
//! the fleet topology.

use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy)]
pub struct ReviewPin<'a> {
    pub reviewed_revision: &'a str,
    pub locked_revision: &'a str,
    pub expected_output: &'a str,
    pub evaluated_output: &'a str,
    pub instance_enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewPinError {
    InvalidRevision,
    RevisionMismatch,
    InvalidOutputPath,
    OutputMismatch,
    ActiveInstance,
    BuildRequired,
    FetchNotProven,
}

impl fmt::Display for ReviewPinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidRevision => "reviewed and locked revisions must be full Git commit IDs",
            Self::RevisionMismatch => "the locked input does not match the reviewed merge commit",
            Self::InvalidOutputPath => "expected and evaluated outputs must be Nix store paths",
            Self::OutputMismatch => "the evaluated output differs from the reviewed package",
            Self::ActiveInstance => "the reviewed staging instance is enabled",
            Self::BuildRequired => {
                "the dry run would build derivations instead of substituting only"
            }
            Self::FetchNotProven => "the dry run did not list the reviewed output as fetched",
        };
        f.write_str(message)
    }
}

impl Error for ReviewPinError {}

fn store_path(path: &str) -> bool {
    let Some((hash, name)) = path
        .strip_prefix("/nix/store/")
        .and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    hash.len() == 32
        && hash
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
}

/// Check the exact lock, evaluated output, and inactive host declaration.
pub fn verify_pin(pin: ReviewPin<'_>) -> Result<(), ReviewPinError> {
    let revision = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !revision(pin.reviewed_revision) || !revision(pin.locked_revision) {
        return Err(ReviewPinError::InvalidRevision);
    }
    if pin.reviewed_revision != pin.locked_revision {
        return Err(ReviewPinError::RevisionMismatch);
    }
    if !store_path(pin.expected_output) || !store_path(pin.evaluated_output) {
        return Err(ReviewPinError::InvalidOutputPath);
    }
    if pin.expected_output != pin.evaluated_output {
        return Err(ReviewPinError::OutputMismatch);
    }
    if pin.instance_enabled {
        return Err(ReviewPinError::ActiveInstance);
    }
    Ok(())
}

/// Accept only a dry run that explicitly fetches the exact output, with no
/// derivations scheduled for local builds. An already-present output is not
/// proof that this cache could restore it on another machine.
pub fn verify_fetch_plan(plan: &str, expected_output: &str) -> Result<(), ReviewPinError> {
    if !store_path(expected_output) {
        return Err(ReviewPinError::InvalidOutputPath);
    }
    let mut fetching = false;
    let mut found = false;
    let mut building = false;
    for line in plan.lines().map(str::trim) {
        if line.starts_with("these derivations will be built:")
            || line.starts_with("don't know how to build these paths:")
        {
            building = true;
            fetching = false;
        } else if line.starts_with("these ")
            && line.contains(" paths will be fetched")
            && line.ends_with(':')
        {
            fetching = true;
        } else if line.ends_with(':') && !line.starts_with("/nix/store/") {
            fetching = false;
        } else if fetching && line == expected_output {
            found = true;
        }
    }
    if building {
        Err(ReviewPinError::BuildRequired)
    } else if !found {
        Err(ReviewPinError::FetchNotProven)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "2701ebc80df3f26c3a9029300df1c2d012b23a22";
    const OUT: &str =
        "/nix/store/2rf8lga86449iqaf4qhyfj4lship7yqn-paperclip-0.3.1-unstable-2026-09-27";

    fn reviewed() -> ReviewPin<'static> {
        ReviewPin {
            reviewed_revision: REV,
            locked_revision: REV,
            expected_output: OUT,
            evaluated_output: OUT,
            instance_enabled: false,
        }
    }

    #[test]
    fn requires_the_reviewed_pin_and_inactive_instance() {
        assert_eq!(verify_pin(reviewed()), Ok(()));
        assert_eq!(
            verify_pin(ReviewPin {
                locked_revision: "087818ad6ad2acf8e89269545dbef118868d92fa",
                ..reviewed()
            }),
            Err(ReviewPinError::RevisionMismatch)
        );
        assert_eq!(
            verify_pin(ReviewPin {
                evaluated_output: "/nix/store/59s6vkyaz4s40b76w7avkigdgyb5yb6r-vm-test-run-paperclip",
                ..reviewed()
            }),
            Err(ReviewPinError::OutputMismatch)
        );
        assert_eq!(
            verify_pin(ReviewPin {
                instance_enabled: true,
                ..reviewed()
            }),
            Err(ReviewPinError::ActiveInstance)
        );
    }

    #[test]
    fn requires_explicit_fetch_without_builds() {
        let fetched = format!("these paths will be fetched (99 MiB download):\n  {OUT}\n");
        assert_eq!(verify_fetch_plan(&fetched, OUT), Ok(()));
        assert_eq!(
            verify_fetch_plan(&fetched.replacen("these paths", "these 2 paths", 1), OUT),
            Ok(())
        );
        assert_eq!(
            verify_fetch_plan("", OUT),
            Err(ReviewPinError::FetchNotProven)
        );
        assert_eq!(
            verify_fetch_plan(
                "these derivations will be built:\n  /nix/store/abc.drv",
                OUT
            ),
            Err(ReviewPinError::BuildRequired)
        );
        assert_eq!(
            verify_fetch_plan(
                &format!("these derivations will be built:\n  /nix/store/abc.drv\n{fetched}"),
                OUT,
            ),
            Err(ReviewPinError::BuildRequired)
        );
        assert_eq!(
            verify_fetch_plan(
                &format!("{fetched}don't know how to build these paths:\n  /nix/store/missing"),
                OUT,
            ),
            Err(ReviewPinError::BuildRequired)
        );
        assert_eq!(
            verify_fetch_plan(&fetched.replace(OUT, &format!("{OUT}-lookalike")), OUT),
            Err(ReviewPinError::FetchNotProven)
        );
    }
}
