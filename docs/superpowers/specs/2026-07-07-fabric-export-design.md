<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026  Epsilon Null Operation -->

# Fabric export & redaction — design (U2)

Recorded 2026-07-07. Second unit of the fabric work, building directly on the U1
collection core ([`2026-07-07-fabric-collection-export-design.md`](2026-07-07-fabric-collection-export-design.md),
which covered U1 + a first sketch of U2). U1 collects read-only artifacts into a
versioned, checksummed store; **U2 turns that store into curated, optionally-redacted,
integrity-stamped bundles** a colleague or a vendor can be handed.

## Motivating use cases (unchanged from U1 spec)

1. **Hand a colleague a directory/tarball** of a curated selection of configs/logs.
2. **Prove a hardware fault to a denying vendor** — a *forensic* bundle: provenance- and
   integrity-stamped, showing what was collected when and what succeeded/failed.

The b3-segment incident bundle assembled by hand on 2026-07-07
(`~/astron/network/incidents/b3-2026-07-07/` — `SUMMARY.md` + per-device artifacts +
tarball) is the **reference output shape and the acceptance target**.

## Decisions (settled)

- **CLI-first.** Build the export engine + a `--fabric-export` CLI mode, headless and
  fully testable, exactly as U1 did with `--fabric-collect`. The tick-box TUI (U1 spec
  §8) becomes a thin later sub-unit over the same engine — out of scope here.
- **Redact by default, `--raw` opt-out.** Shareable bundles scrub secrets by default; a
  printed preview reports what was masked (never the secret itself); `--raw` keeps full
  config for a trusted vendor under NDA and is **recorded in the bundle** so a recipient
  knows it is unredacted.
- **Presets reuse the profile `bundle` tags.** `support` / `forensic` / `config` already
  tag artifacts in the vendor profiles; an export preset is just a bundle-tag filter.
- **Store stays raw.** Redaction is an export-time transform only (U1 decision); the
  local store is never mutated by an export.

## Scope & boundary

U2 = **redaction** (pure) + **export** (select from the store, redact, write a
dir/tarball with provenance + integrity). It does **not** include the interactive TUI
(its own later plan), cryptographic signing (checksums only), or any change to the
collection path. It reads the store U1 writes and never mutates it.

## Architecture — two new modules + CLI wiring

| Module | Responsibility | Depends on |
|---|---|---|
| `fabric/redact.rs` | **Pure** `redact(format, text) -> (masked, Vec<Redaction>)`; per-format secret patterns; a `Redaction{line,kind}` record for the preview | — (pure, unit-tested) |
| `fabric/export.rs` | Resolve a selection from the store, apply redaction, write a dir or `.tar.gz` with `SUMMARY.md` + `CHECKSUMS.sha256` + copied manifests | `store`, `redact`, `profile` |
| `src/main.rs` | Flat `--fabric-export` mode + selection/redaction flags (canopy idiom, like `--fabric-collect`) | export |

New dependencies: `tar` + `flate2` (both pin majors) for `.tar.gz`; `sha2` (already present)
for the bundle checksums.

### Redaction (`fabric/redact.rs`)

```rust
/// One masked secret — WHERE and WHAT KIND, never the secret value itself.
pub struct Redaction { pub line: usize, pub kind: String }

/// Scrub secrets from `text`, returning the masked text and a list of what was masked.
/// `format` is the artifact's profile `format` tag (`junos`, `junos-set`, or None for a
/// conservative generic pass). Pure and unit-tested against real Junos secret forms.
pub fn redact(format: Option<&str>, text: &str) -> (String, Vec<Redaction>);
```

Junos pattern set (first cut), each replaced with a stable placeholder
(`"<REDACTED:secret>"`):

- `secret "$9$…"`, and `$1$…`/`$5$…`/`$6$…` password hashes;
- `snmp … community "…"` and `community <name>`;
- `authentication-key "…"`, `auth-password`, `key "…"` under security/BGP-MD5;
- `pre-shared-key "…"`, `pre-shared-key ascii-text`;
- SSH/host key blobs (`ssh-rsa …`, `ssh-ed25519 …`);
- RADIUS/TACACS `secret "…"`.

The generic (unknown-format) pass masks the vendor-independent forms (key blobs, obvious
`password`/`secret`/`community` assignments) conservatively. **The redaction test is a
round-trip: the masked output contains none of the seeded secrets, and the `Redaction`
list matches the seeded count** — verified against real config fixtures.

### Export (`fabric/export.rs`)

Selection resolves against the store: **device(s) × snapshot(s) × artifact(s)**.

```rust
pub struct Selection {
    pub devices: Vec<String>,   // empty = all devices under the site in the store
    pub preset: Option<String>, // bundle tag: support|forensic|config; None = all artifacts
    pub snapshot: SnapshotSel,  // Latest | Exact(ts) | Since(ts)
}
pub struct ExportOpts { pub redact: bool /* default true */, pub out: PathBuf }

/// Build the bundle; returns a summary (files written, secrets masked, artifacts by
/// exit status). Writes a directory, or a `.tar.gz` when `out` ends in `.tar.gz`.
pub fn export(store: &Store, site: &str, sel: &Selection, opts: &ExportOpts) -> Result<ExportReport>;
```

Bundle layout (mirrors the store, plus generated top-level files):

```
<bundle>/
  SUMMARY.md                      # generated: devices, snapshots, per-device clock skew,
                                  #   failed-artifact counts, redaction status (masked N)
  CHECKSUMS.sha256                # sha256 of every file actually shipped in the bundle
  <device>/<snapshot-ts>/
     manifest.json               # copied from the store (collection provenance + raw sha256s)
     <artifact>.txt              # redacted (default) or raw (--raw) body
```

Integrity nuance (important for the vendor case): `CHECKSUMS.sha256` is computed over the
**exported** files. When redaction ran, a redacted `<artifact>.txt` will NOT match the
raw sha256 in the copied `manifest.json` — `SUMMARY.md` states this ("redacted: N secrets
masked across M files"), so a recipient understands the shipped files were scrubbed. A
`--raw` bundle's exported checksums DO match the store manifest, giving unbroken
chain-of-custody from device to bundle.

### CLI (`src/main.rs`, flat flags — canopy idiom)

- `--fabric-export <OUT>` — mode trigger; `OUT` is a directory, or a `*.tar.gz` file.
- `--export-device <NAME>` (repeatable; default = all devices in the store for the site).
- `--export-preset <support|forensic|config>` (default = all artifacts present).
- `--export-snapshot <latest|TS>` (default `latest`) and/or `--export-since <TS>`.
- `--raw` — disable redaction (recorded in `SUMMARY.md`).
- Reuses `--site`; store root from `Store::default_root()` (a `--fabric-store <DIR>`
  override exists for tests). Prints the redaction preview + the bundle path.

## Safety

- Redaction default-on for shareable bundles; `--raw` is explicit and recorded in the
  bundle so unredacted content is never mistaken for scrubbed.
- The preview and `Redaction` records report location + kind only — **never the secret
  value** (so logs/stdout never leak what was masked).
- The store is read-only to export; provenance and per-artifact raw checksums travel with
  the bundle via the copied `manifest.json`.

## Testing (headless, canopy discipline)

- `redact`: round-trip against real Junos secret fixtures (masked output has no secret;
  `Redaction` list matches); each pattern has a positive and a negative case.
- `export`: build a bundle from a fixture store in a tempdir — assert the layout,
  `SUMMARY.md` content, `CHECKSUMS.sha256` correctness, redaction applied (and `--raw`
  not applied), and `.tar.gz` vs directory output.
- Selection resolution: device/preset/snapshot filters pick the right files.
- Acceptance: a bundle whose shape matches the hand-built b3 incident bundle.

## Build order (CLI-first; each step shippable & testable)

1. **Redaction** (`redact.rs`) + its round-trip tests — pure, no store needed.
2. **Selection + store reader** (list devices/snapshots, resolve a `Selection`).
3. **Export to a directory** (redact, copy manifests, write `SUMMARY.md` + `CHECKSUMS.sha256`).
4. **Tarball output** (`.tar.gz` when `out` ends in `.tar.gz`).
5. **`--fabric-export` CLI** wiring + preview.
   → gives `canopy --fabric-export bundle.tar.gz --export-preset support`, testable
   against the live store populated by `--fabric-collect`.

## Out of scope (future units)

The interactive tick-box TUI (U1 spec §8) — its own plan over this engine. Cryptographic
signing of bundles (checksums only for now). U3 discovery / U4 map are unaffected.
