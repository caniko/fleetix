# Paired dev/prod service surfaces

Fleetix's opt-in service-surfaces backend runs two complete, persistent NixOS
service guests on a machine. Databases, workers, mail, DNS, VPN and ingress
belong inside the guests, not in an implicitly shared application dependency
layer. Machine boot and control remain on the host.

The release is one **already-realized guest system output**. Dev and prod boot
the same store path with different root filesystems, machine identities,
credentials and runtime bindings. Promotion changes prod's selected build;
it never exchanges containers, copies dev data, or rebuilds from a source ref.
Guest restarts are intentional: this is not a zero-downtime or host-HA promise.

## Configure the guest once

Build a standalone NixOS system importing
`fleetix.nixosModules.service-surface-guest`, plus the complete workload modules.
Set:

```nix
fleetix.serviceSurface = {
  services = ["database" "web" "worker"];
  healthCheck = "${fullStackHealth}/bin/check";
};
```

`healthCheck` must check application readiness, databases and background jobs,
not just an HTTP landing page. It executes inside the guest with arguments
`SURFACE EXPECTED_SYSTEM_OUTPUT`. The guest embeds the executable and a
versioned `fleetix-surface.json` inventory, including every enabled system unit.
The logical service inventory must exactly match the host policy.

Do not compile dev/prod addresses, domains or credentials into two different
guest outputs. The host supplies addresses before guest init and mounts:

- `/run/fleetix-surface/bindings.json`: non-secret `{version, host, surface,
  bindings}` data, including surface-specific domain names and callback URLs;
- `/run/fleetix-surface/credentials`: that surface's read-only runtime secrets.
- `/run/fleetix-surface/resolv.conf`: explicit per-surface DNS configuration;
  `/etc/resolv.conf` points here rather than inheriting the host's loopback stub.

Configure applications to read those runtime bindings. Both guests use the
neutral hostname `fleetix` in separate namespaces. Application state paths may
be identical *inside* the guests: their persistent root filesystems are separate.

## Configure the host

Import `fleetix.nixosModules.service-surfaces`. Supply a complete service list,
an immutable **operator-owned admission executable**, explicit addresses, and
disjoint credential directories:

```nix
fleetix.serviceSurfaces = {
  enable = true;
  services = ["database" "web" "worker"];
  admissionCheck = "${deploymentPolicy}/bin/admit";
  dev = {
    hostAddress = "10.233.1.1";
    localAddress = "10.233.1.2";
    credentialsDirectory = "/run/service-credentials/dev";
  };
  prod = {
    hostAddress = "10.233.2.1";
    localAddress = "10.233.2.2";
    credentialsDirectory = "/run/service-credentials/prod";
  };
};
```

The addresses above are a fixture, not a fleet allocation. Choose nonconflicting
addresses from the consumer's topology. Optional `Host.serviceSurfaces` Pkl
metadata supplies the service inventory and container names; topology's existing
`Deployment` ingress/intent data does not become a runtime journal.

The default names are `fleetix-dev` and `fleetix-prod`. Root filesystems remain
under `/var/lib/nixos-containers/`; the private journal, GC roots and selected
system links live under `/var/lib/fleetix-surfaces/`. Do not clone rootfs trees or
manually edit these links or the journal. Changing policy invalidates historical
operation authorization and requires a separately reviewed migration.

Dev has a default 2 GiB memory ceiling and 100% CPU quota. Size those limits for
the actual full stack. It also has a 1024-task ceiling and lower relative IO
weight. Production's CPU quota is omitted, not the invalid value `infinity`.
Both surfaces have private networks and user namespaces;
neither receives the host Nix daemon socket. Dev forwarding is denied, including
routes to prod, and unsolicited guest access to the host is denied. Access dev
through the host (for example an SSH tunnel). Production egress requires an
explicit `productionEgressInterface` and `prod.nameservers`; public ingress and external integrations
remain consumer-owned bindings. Dev must use sandbox/sink destinations, never
production credentials or delivery targets.

Set `enableTun = true` only when both stacks contain VPN services requiring
TUN/network-administration capability. Runtime bindings, addresses, resolver and
credential changes take effect on the next controlled restart/promotion, not an
unrelated host rebuild. Credential directories must be outside container roots.
All `/nix` bind sources and credentials need filesystems supporting idmapped
mounts. The qualification VM uses a native ext4 root disk instead of the default
shared/overlay store, supporting host Nix database registration while the guest
store stays read-only and idmapped.
Preserve numeric ownership in rootfs backups: nspawn's picked user-namespace
range is recorded in ownership.

Containers share the host kernel and read-only Nix store. They are not a boundary
for hostile kernel workloads. Exclusive devices, physical networking and services
that cannot use independent state need an explicit supported adaptation; admission
must reject the host until *all* configured services are covered. Installing this
module does not move existing host services or migrate their data.

## Admission and data safety

`admissionCheck` runs on the **host**, independently of candidate code, with:

```text
OPERATION SURFACE PREVIOUS_OUTPUT_OR_EMPTY CANDIDATE_OUTPUT CONTAINER_NAME
```

An `abort` of a never-activated operation passes the retained baseline as
PREVIOUS (empty for an initial install) and the cancelled candidate.

It must be read-only and idempotent. It must verify live schema compatibility,
backup/recovery readiness, required service coverage, runtime-binding isolation,
capacity and hardware support. Missing evidence must fail closed. It must also
authorize recovery and rollback against the *current data*, not assume the old
binary can read a newly migrated database. There is deliberately no default
successful admission hook and no automatic destructive migration engine.

Apply separately authorized migrations with the application's existing guarded
mechanism. A NixOS generation rollback restores code/configuration, **not data**.

## Operate exact outputs

The standalone binary requires Cargo feature `service-surfaces-cli`; the pure
contract is available without Nix, and `service-surfaces-runtime` exposes the
durable runtime library. The production Fleetix package includes the binary.
Run host operations as root; the production journal is intentionally private.

```sh
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json status
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json deploy-dev /nix/store/OUTPUT-nixos-system-fleetix
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json promote --expected-dev /nix/store/OUTPUT-nixos-system-fleetix
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json recover
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json resume
fleetix-surfaces --policy /etc/fleetix-surfaces/policy.json rollback
```

Use an actual realized store output, not the placeholder above, a `.drv`, a flake
ref or a mutable result link. Build and transfer it separately through the
consumer's normal build/cache workflow. Promotion never evaluates or compiles.

Promotion requires the expected dev output, fresh live dev health and an unchanged
prod baseline. A write-ahead journal blocks another operation after interruption;
explicit recovery consults the same independent admission policy. Prior outputs
remain rooted. Before any activation intent, a failed admission can be cancelled
through guarded `recover`, which requires the live surface to still match its
journaled baseline: the previous release, or confirmed inactivity with no
selected release for an initial install. After activation intent, an initial
install has no rollback baseline: use guarded `resume` to forward-complete its
exact candidate. Unknown live state never counts as an empty slot. Known stopped
deployments can be started through `boot` before a new deployment; outstanding
systemd jobs must finish first. Host reboot starts only committed surfaces under
the configured host activation lock; a pending operation blocks only its own
surface, and the other surface still starts with the refusal reported.

All host-activation frontends must share `lockPath`. Never remove a persistent
lock anchor to clear contention. Set `manageLockAnchor = false` when the host
already owns that exact anchor; Canix does this and rejects a divergent lock path.
Existing root-owned read-only anchors are supported without changing their mode.
The Linux native backend requires pidfds and `close_range(CLOEXEC)` (Linux 5.11+
or equivalent backports). Command payloads receive no extra host descriptors or
inherited host environment (only a trusted PATH and explicit admission context);
a host-only keeper retains the calling thread's lease across worker crashes,
bounded by the command timeout plus one second. Guest code cannot unlock or
indefinitely retain that lease. PID 1's outstanding container jobs are checked
separately because killing a `systemctl` client does not cancel them.
The native backend deliberately uses stable
profile links and `systemctl`, **not** `nixos-container update`: the latter builds
from source and ignores `--system-path` during updates. See the authoritative
[container module](https://github.com/NixOS/nixpkgs/blob/nixos-unstable/nixos/modules/virtualisation/nixos-containers.nix)
and [container CLI](https://github.com/NixOS/nixpkgs/blob/nixos-unstable/pkgs/by-name/ni/nixos-container/nixos-container.pl).

## Qualification and downstream adoption

Focused Rust tests exercise exact-artifact promotion, drift, failed admission,
interruption and guarded recovery. `checks.<system>.service-surfaces-module`
checks the NixOS contracts. `checks.<system>.service-surfaces-vm` exercises real
containers, PostgreSQL, workers, network fencing, isolated credentials, rollback
and reboot. Evaluate fixtures locally; run the heavy VM/package gates in PR CI.

Toolbelt's `nixosModules.service-surfaces` aliases its options to this engine.
Consumers must first pin a published Fleetix revision exporting the module.
Canix's prepared adapter retains `/run/lock/canix/switch.lock`; it is intentionally
not imported by current host defaults before published inputs and service/data
migration are qualified. No local-checkout dependency or second promoter is used.
