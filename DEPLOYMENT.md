# Recoverable deployment

Fleetix's opt-in deployment worker supports NixOS and standalone Home Manager.
It does not turn arbitrary configuration changes into safe unattended updates.

The lifecycle is **admit → retain recovery targets → trial → verify → commit**.
A failed or interrupted trial restores the original identities and verifies the
restoration. Recovery never recaptures a trial candidate as the old generation.

## Authority

Install `fleetix-deploy` in a target-local system or user service with a fixed,
administrator-controlled policy path. Do not grant passwordless access to
`fleetix-deploy` with caller-selected policies. Requests nominate a candidate;
they cannot supply commands, privilege, health checks or approval flags.

An installed admission verifier must approve the exact request and observed
previous generations. Its receipt is bound to the candidate, target, backend,
request digest and expiry. The verifier is responsible for trusted source and
qualification evidence, configuration-only effects and rollback compatibility.
Unknown effects, destructive hooks and incompatible state migrations must be
refused. Root ownership of a Nix store object does **not** authorize its code.

No-sudo activation uses the same request/worker boundary, not a second approval
path. Toolbelt's opt-in `requestUsers` grants named users write access to a
dedicated watched inbox. Publishing a request starts the fixed root worker;
the user cannot select its privileged policy or argv. Filesystem access permits
nomination only: the installed verifier still decides eligibility. Home Manager
uses the equivalent same-owner user service. No generic sudo/polkit grant is
needed, and private recovery state remains inaccessible to system submitters.

The health command is also fixed by policy. It must cover the deployed service
and its dependencies, not merely successful activation or a running timer.
Commands use explicit argv, have deadlines, and never prompt for credentials.
Admission must remain valid for the configured activation budget after GC-root
retention finishes; insufficient lifetime is refused before activation.

## Recovery boundary

NixOS trials use `switch-to-configuration test`, leaving the boot profile
unchanged. Only verified candidates are persisted. Recovery restores both the
previous running system and the previous boot profile; these can differ.
Home Manager trials activate the candidate generation and retain the previous
generation for restoration.

Both generations, the worker and its policy are GC-rooted before mutation.
The private, fsynced journal is written before each mutating phase. Keep a
target-local failure/boot recovery service installed; the deploying terminal
is not the recovery controller. All activation frontends must use the same
configured kernel lock. Never delete its anchor to resolve contention.

Generation rollback restores managed configuration, **not arbitrary data**.
Do not admit a candidate unless the previous version can safely use the state
left by the new version. A backup is not permission to discard acknowledged
writes. Database upgrades, destructive migrations, secret rotations and changes
to the recovery/authorization substrate require a separate qualified maintenance
path. Fleetix does not initialize or restore application databases itself.

The worker refuses to overwrite an unrelated generation during recovery. A
failed restoration is recorded as a recovery failure and needs operator action;
it is never reported as a healthy deployment.

Both run and recovery commands exit 75 only when the activation lock is busy.
Recovery units must accept that code as a deferred scan, not a failed service
that would poison an otherwise healthy NixOS activation. The periodic timer
retries the scan. A deferred scan does not certify restoration; genuine recovery
failures still exit 1 and remain visible.

Trigger NixOS workers at boot through timers, not `multi-user.target.wants`.
The switcher starts active targets and otherwise waits for its own activation
oneshot. Set `restartIfChanged = false` so changing a worker unit cannot kill its
running transaction. NixOS also resets failed-unit state during activation;
terminal journal state and health, not a failed-unit flag, confirm restoration.

Inspect `fleetix-deploy status --policy POLICY` before an explicit maintenance
retry with `fleetix-deploy recover --policy POLICY --retry-failed`. Normal
recovery timers never clear a failed-restoration terminal. An unrelated current
generation is never overwritten, including during an explicit retry.

The worker handles INT/TERM/HUP by settling the current child before restoration.
Install it with `KillMode=control-group` as well: SIGKILL cannot be handled, and
process groups are not a substitute for the service's descendant containment.
If a child cannot be reaped after termination, automatic restoration is withheld
and the journal records the need for operator inspection.

## Integration

Build the `deployment-cli` feature, or use the `fleetix-deploy` flake package.
The pure contract API remains usable with `default-features = false`.
`canix-toolbelt` supplies optional fixed-policy system and user units. Fleet
inventory, allowed effects and application-specific verifiers remain consumer
policy; Harbor-DB retains ownership of database custody and compatibility.

This feature does not migrate existing integrated Home Manager profiles or
enable unattended deployment on a host. Such adoption needs its own reviewed
policy and restoration qualification. There is no Canix runtime dependency.
