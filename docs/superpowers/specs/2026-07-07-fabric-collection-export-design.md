<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026  Epsilon Null Operation -->

# Fabric collection & export — design (U1 + U2)

Recorded 2026-07-07. First concrete step of the vision's "Beyond DNS: switches and
routers" chapter ([`vision.md`](../../vision.md)). This spec covers **collecting**
read-only diagnostic artifacts from network devices and **exporting** curated,
optionally-redacted, integrity-stamped bundles. It deliberately stops short of parsing,
discovery, and the map — those are later units.

## Context

canopy already has the substrate this builds on:

- **SSH access** — `src/sources/vantage.rs` shells out to the system `ssh`, honouring
  `~/.ssh/config`, keys, and `-J` ProxyJump chains (direct or through a bastion).
- **Config-as-data idiom** — the site TOML already declares `[[dns_servers]]` with
  per-server `jump`/`vantage` overrides; the device inventory mirrors it.
- **Discipline** — read-only by default, `--dry-run`, diff-before-apply, secrets from
  `pass`, headless render/unit tests.

This work reuses all of the above rather than inventing parallel machinery.

## The larger picture (for orientation only — NOT in this spec)

The full ambition is four units; this spec is U1 + U2:

| Unit | What it is |
|---|---|
| **U1 — Fabric vantage & collection** | Read-only SSH collection into a versioned per-device store |
| **U2 — Selectable export** | TUI tick-box selection → directory / tarball for a colleague or vendor |
| U3 — Discovery & segmentation | Parse configs + LLDP/CDP/ARP/routing → find neighbours, recurse logins, infer segmentation & reachability |
| U4 — Multi-layer map | mullion node-graph rendering physical / logical / VLAN / reachability layers |

U2 needs U1's store; U3 parses U1's configs; U4 draws U1+U3's data. Each gets its own
spec → plan → build cycle.

## Motivating use cases

1. **Hand a colleague a directory/tarball** of a curated selection of configs and logs.
2. **Prove a hardware fault to a denying vendor.** The bundle must be *forensic*:
   provenance-stamped (when collected, device-clock skew, that it was read-only, via which
   path), integrity-stamped (per-artifact checksums, tamper-evident), and rich in the
   evidence that pins hardware — error counters (CRC/FEC/input), optics DOM, FPC/PIC/line-
   card alarms and state — sampled **repeatedly over time** to show a trend. This is the
   real, current driver: proving a malfunctioning line card beyond doubt.

The b3-segment incident bundle assembled by hand on 2026-07-07
(`~/astron/network/incidents/b3-2026-07-07/`) is the reference output shape and doubles as
an acceptance test.

## Decisions (settled during brainstorming)

- **Access:** reuse `Vantage` (shell out to system `ssh`); no new SSH layer.
- **Inventory:** a `[[device]]` array in the site TOML, parallel to `[[dns_servers]]`.
- **Collect:** all four artifact bundles (identity, config, topology+L2/L3 state,
  logs/alarms/optics), fully selectable so operators curate what to send.
- **Secrets:** **redact at export.** Store raw locally under tight perms; scrub secrets
  only when building a *shareable* bundle, with a preview and a per-export raw toggle.
- **History:** **versioned snapshots** (timestamped, `latest` pointer) — required for the
  trend-over-time evidence goal.
- **Vendor model:** **data-driven TOML profiles** (artifact → command + bundle tags),
  extensible without recompiling.

## Scope & boundary

U1+U2 **collects raw artifacts, stores them versioned, and exports curated (optionally
redacted, integrity-stamped) bundles.** It does **not** parse configs into a model, build
the graph, or discover neighbours (U3/U4). The only interpretation U1 performs is: vendor
detection from `show version`, redaction pattern-matching, and checksums. `reconcile.rs`
and the existing graph/map code are untouched.

## Architecture — new `src/fabric/` subsystem

Mirrors canopy's existing module style (pure core + thin I/O + a `tui/` view).

| Module | Responsibility | Depends on |
|---|---|---|
| `fabric/inventory.rs` | `[[device]]` model (name, host/ip, jump, user, os); load + merge from site TOML; hand back a `Vantage` per device | `sources::vantage` |
| `fabric/profile.rs` | Data-driven vendor profiles (TOML): artifact → `{command, bundle, format, heavy}`; ships `junos-evo` + `junos`; vendor auto-detect from `show version` | pure |
| `fabric/collect.rs` | Orchestrate: run a device's selected artifacts over `Vantage`, write a snapshot + manifest | inventory, profile, store, vantage |
| `fabric/store.rs` | Versioned on-disk store: paths, `latest`, `manifest.json` (provenance + per-artifact sha256) | — |
| `fabric/redact.rs` | Pure secret-scrub pass per format + a preview/diff of what is masked | pure |
| `fabric/export.rs` | Build a dir/tarball from a selection (device × artifact × snapshot); apply redaction; presets; integrity file | store, redact |
| `tui/fabric.rs` | TUI view: Site→Device→Snapshot→Artifact tree with tick-boxes, presets, Collect / Export actions | mullion, above |

### Inventory & access

```toml
[[device]]
name = "acx-a2-0"
host = "10.155.251.23"        # or a DNS name honoured by ~/.ssh/config
# jump = "bastion.astron.nl"  # per-device; falls back to the site-wide `jump`
os   = "junos-evo"            # optional; auto-detected from `show version` on first collect
```

Seeded from the 15-host ASTRON/LOFAR list already inventoried under
`~/astron/network/`.

### Collection & profiles (data-driven)

A profile is data, so command sets are curated without recompiling (the block below is
schematic — the real file uses one `[artifact.<name>]` table per artifact with its keys on
following lines):

```toml
# profiles/junos-evo.toml  (schematic)
[artifact.version]     cmd = "show version"                       bundle = ["identity","support","forensic"]
[artifact.config-set]  cmd = "show configuration | display set"   bundle = ["config"]    format = "junos-set"
[artifact.optics]      cmd = "show interfaces diagnostics optics" bundle = ["forensic"]
[artifact.fpc-errors]  cmd = "show interfaces extensive | match \"error|CRC|FEC|drops\""  bundle = ["forensic"]
[artifact.rsi]         cmd = "request support information"        bundle = ["support"]   heavy = true
```

- `bundle` tags drive the export presets and TUI tick-boxes.
- `heavy = true` (e.g. RSI) gets a longer timeout + streaming and is opt-in.
- All commands are read-only verbs (`show` / `request … information` / `file show`);
  a config-changing verb in a profile is a validation error.

### Store, provenance & integrity (the forensic backbone)

```
~/.local/share/canopy/fabric/<site>/<device>/<UTC-timestamp>/<artifact>.txt
                                     .../<device>/latest -> <timestamp>
                                     .../<device>/<timestamp>/manifest.json
```

`manifest.json` per snapshot is what makes a bundle *evidence*:

- provenance: `collected_at` (real clock), `device_clock` + computed **skew**, `via`
  (jump path), `user`, `canopy_version`, `read_only: true`;
- per artifact: exact `command`, `sha256`, `bytes`, `exit`.

Versioned snapshots + this manifest give trend + tamper-evidence to face a denying vendor.
(Device-clock skew is itself evidence — cf. `ex-b1-1`, found ~2.5 years behind.)

### Redaction (redact-at-export)

Store is raw (files `0600`, dirs `0700`). A pure
`redact(format, text) -> (masked, Vec<Redaction>)` runs only when building a *shareable*
export. Per-format patterns cover Junos secret forms: `secret "$9$…"`,
`authentication-key`, `snmp … community`, `pre-shared-key`, `encrypted-password`, and key
blobs. The TUI shows a **preview** (what/how many masked, by line) and a **per-export
toggle** to keep raw for a trusted vendor under NDA.

### Export & presets

Selection = any set of (device × artifact × snapshot). Output = a directory or `.tar.gz`
containing `SUMMARY.md` + `CHECKSUMS.sha256`. Three presets seed the tick-boxes:

- **support** — RSI + logs + state (what JTAC asks for);
- **forensic** — identity + env + optics DOM + error counters + FPC/PIC state + alarms +
  logs; designed to be re-run to build a trend (the line-card-proof case);
- **à la carte** — any subset.

### TUI view

A `mullion` tree — Site → Device → Snapshot → Artifact, each a checkbox; presets set the
checkboxes; `[c]` collects the selection into a new snapshot (progress via
`Vantage::run_streaming`); `[e]` exports checked items (pick redacted/raw → preview → write
tarball). Reuses the existing `tui/` focus/tree idiom.

## Safety

- Read-only by default; collection verbs are allow-listed; a config-changing verb in a
  profile fails validation.
- Redaction default-on for shareable exports; the raw toggle is explicit and recorded.
- Secrets are never logged; store perms are tight; provenance records `read_only: true`.

## Testing

Headless unit tests, in canopy's discipline (no tty, no live device):

- profile TOML parse + validation (rejects a config-changing verb);
- redaction against real Junos secret forms (round-trip: masked output has no secret, and
  the `Redaction` list matches);
- store path construction, `latest` pointer, manifest + sha256;
- export selection → tarball in a tempdir (contents, `CHECKSUMS.sha256`, redaction applied);
- inventory load/merge from site TOML.

Live collection is exercised manually against the 8 reachable ASTRON/LOFAR devices.

## Build order (CLI-first, like `canopy --list`; each step shippable & testable)

1. **Inventory** model + `Vantage` wiring + vendor detection.
2. **Profiles** (`junos-evo`, `junos`) + `collect` orchestrator.
3. **Store**: versioning, `latest`, provenance + checksums.
   → gives `canopy fabric collect`, testable on the 8 live devices.
4. **Redaction** pass + preview.
5. **Export**: selection → dir/tarball + presets + integrity file.
   → gives `canopy fabric export`.
6. **TUI** fabric view wiring collect + export.

## Out of scope (future units)

Config parsing into a model, neighbour discovery, segmentation/reachability inference,
device-to-device jump-chaining discovered from configs, and the multi-layer node-graph
map. Also out of scope for v1: cryptographic signing of bundles (checksums only), and any
config **write** path (which the vision gates behind full AAA).
