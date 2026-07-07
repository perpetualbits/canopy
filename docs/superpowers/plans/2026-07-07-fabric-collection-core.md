# Fabric Collection Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `canopy fabric collect` command that runs read-only diagnostic commands on a network device over SSH and writes a versioned, provenance- and checksum-stamped snapshot to an on-disk store.

**Architecture:** A new `src/fabric/` subsystem with a pure core (inventory model, data-driven vendor profiles, snapshot store + manifest) and one thin I/O seam (`CommandRunner`, implemented for the existing `sources::vantage::Vantage`). Collection is orchestrated by a function that takes an injected `CommandRunner`, so the whole pipeline is unit-testable headless with a fake runner; production passes a `Vantage` that shells out to system `ssh`.

**Tech Stack:** Rust 2021, `serde`/`toml`/`serde_json` (already present), `sha2` (checksums), `chrono` (UTC timestamps + device-clock skew), `tempfile` (dev-only, store tests), `mullion`/`clap`/`anyhow` (already present).

**Scope:** Build steps 1–3 of `docs/superpowers/specs/2026-07-07-fabric-collection-export-design.md`. Redaction + export (steps 4–5) and the TUI view (step 6) are deliberately out of scope here and become follow-on plans. Nothing in `reconcile.rs` or the existing graph/map code is touched.

## Global Constraints

- Rust 2021, `rust-version = 1.85`. Keep it compiling and **warning-free** after every task.
- **Licence header on every new file:** `// SPDX-License-Identifier: GPL-3.0-or-later` then `// Copyright (C) 2026  Epsilon Null Operation`. (TOML data files use `#` comment form.)
- **Doc-comment every public item** (`///`) and every module (`//!`): what it does, and for logic, how/why.
- **No `unwrap()`/`expect()`** outside tests and `main`/startup. Use `anyhow::Result`.
- Pin dependency majors in `Cargo.toml`; no `"*"`.
- **Read-only by default:** collection only ever runs allow-listed verbs; a config-changing command in a profile is a validation error.

---

### Task 1: Device inventory

**Files:**
- Create: `src/fabric/mod.rs`
- Create: `src/fabric/inventory.rs`
- Modify: `src/main.rs` (add `mod fabric;` near the other `mod` declarations)

**Interfaces:**
- Consumes: `sources::vantage::Vantage` (existing: `Vantage::with_jump(host, jump)`).
- Produces:
  - `pub struct Device { pub name: String, pub host: String, pub jump: Option<String>, pub user: Option<String>, pub os: Option<String> }`
  - `pub struct Inventory { pub devices: Vec<Device> }`
  - `Inventory::from_toml_str(s: &str) -> anyhow::Result<Inventory>`
  - `Inventory::load(path: &std::path::Path) -> anyhow::Result<Inventory>`
  - `Inventory::get(&self, name: &str) -> Option<&Device>`
  - `Device::vantage(&self, site_jump: &str) -> Vantage` (per-device `jump` wins; else `site_jump`)
  - `Device::ssh_host(&self) -> String` (`user@host` when `user` is set, else `host`)

- [ ] **Step 1: Add the module and wire it into the crate**

In `src/main.rs`, add alongside the existing module declarations:

```rust
mod fabric;
```

Create `src/fabric/mod.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The **fabric** subsystem: read-only collection of diagnostic artifacts from
//! network devices (switches/routers) into a versioned on-disk store. Pure model
//! (`inventory`, `profile`, `store`) plus one thin I/O seam (`collect`'s
//! `CommandRunner`, implemented for `sources::vantage::Vantage`).

pub mod inventory;
```

- [ ] **Step 2: Write the failing test**

Create `src/fabric/inventory.rs` with only its `#[cfg(test)]` module first (so it fails to compile → fails):

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [[device]]
        name = "acx-a2-0"
        host = "10.155.251.23"

        [[device]]
        name = "lofar-core-router-re0"
        host = "10.155.250.1"
        jump = "bastion.astron.nl"
        user = "nagtegaal"
        os   = "junos-evo"
    "#;

    #[test]
    fn parses_devices_and_optional_fields() {
        let inv = Inventory::from_toml_str(SAMPLE).unwrap();
        assert_eq!(inv.devices.len(), 2);
        let core = inv.get("lofar-core-router-re0").unwrap();
        assert_eq!(core.host, "10.155.250.1");
        assert_eq!(core.jump.as_deref(), Some("bastion.astron.nl"));
        assert_eq!(core.os.as_deref(), Some("junos-evo"));
        assert!(inv.get("acx-a2-0").unwrap().jump.is_none());
    }

    #[test]
    fn per_device_jump_wins_else_site_default() {
        let inv = Inventory::from_toml_str(SAMPLE).unwrap();
        // device with its own jump keeps it
        let core = inv.get("lofar-core-router-re0").unwrap();
        assert_eq!(core.vantage("site-default").jump, "bastion.astron.nl");
        // device without one falls back to the site jump
        let a2 = inv.get("acx-a2-0").unwrap();
        assert_eq!(a2.vantage("site-default").jump, "site-default");
    }

    #[test]
    fn ssh_host_includes_user_when_set() {
        let inv = Inventory::from_toml_str(SAMPLE).unwrap();
        assert_eq!(inv.get("lofar-core-router-re0").unwrap().ssh_host(), "nagtegaal@10.155.250.1");
        assert_eq!(inv.get("acx-a2-0").unwrap().ssh_host(), "10.155.251.23");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p canopy fabric::inventory`
Expected: FAIL — compile error, `Inventory`/`Device` not found.

- [ ] **Step 4: Write the minimal implementation**

Prepend to `src/fabric/inventory.rs` (above the test module):

```rust
//! The device **inventory**: the `[[device]]` array in a site TOML file, and how
//! each device is reached (per-device `jump`, falling back to the site jump).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::sources::vantage::Vantage;

/// One network device we can collect from.
#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    /// Stable short name, e.g. `acx-a2-0`. Used as the store directory name.
    pub name: String,
    /// SSH destination — an IP or a name honoured by `~/.ssh/config`.
    pub host: String,
    /// Optional per-device `ProxyJump` chain; empty/absent falls back to the site jump.
    #[serde(default)]
    pub jump: Option<String>,
    /// Optional SSH username; absent means `~/.ssh/config` / current user decides.
    #[serde(default)]
    pub user: Option<String>,
    /// Optional platform id (`junos-evo`, `junos`); auto-detected on first collect if absent.
    #[serde(default)]
    pub os: Option<String>,
}

impl Device {
    /// The SSH host string, `user@host` when a user is configured, else `host`.
    #[must_use]
    pub fn ssh_host(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// A [`Vantage`] for reaching this device: the per-device `jump` if set, else
    /// `site_jump` (which may itself be empty for a direct connection).
    #[must_use]
    pub fn vantage(&self, site_jump: &str) -> Vantage {
        let jump = self.jump.clone().unwrap_or_else(|| site_jump.to_string());
        Vantage::with_jump(self.ssh_host(), jump)
    }
}

/// The parsed `[[device]]` inventory from a site TOML file.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Inventory {
    /// All devices, in file order.
    #[serde(default, rename = "device")]
    pub devices: Vec<Device>,
}

impl Inventory {
    /// Parse an inventory from TOML text (the site file, or a fragment).
    ///
    /// # Errors
    /// Fails if the text is not valid TOML or a `[[device]]` entry is malformed.
    pub fn from_toml_str(s: &str) -> Result<Inventory> {
        toml::from_str(s).context("parsing device inventory TOML")
    }

    /// Load an inventory from a site TOML file on disk.
    ///
    /// # Errors
    /// Fails if the file cannot be read or does not parse.
    pub fn load(path: &Path) -> Result<Inventory> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading inventory file {}", path.display()))?;
        Self::from_toml_str(&text)
    }

    /// Find a device by its `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.name == name)
    }
}
```

Then add to `src/fabric/mod.rs` nothing new (already declares `pub mod inventory;`).

> Note: this reads the site file's `[[device]]` array directly, independent of the main config loader, so it is testable without the rest of `config.rs`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p canopy fabric::inventory`
Expected: PASS (3 tests). Then `cargo build 2>&1 | grep -c warning` → `0`.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/fabric/mod.rs src/fabric/inventory.rs
git commit -m "fabric: device inventory model + per-device vantage"
```

---

### Task 2: Data-driven vendor profiles

**Files:**
- Create: `src/fabric/profile.rs`
- Create: `profiles/junos-evo.toml`
- Create: `profiles/junos.toml`
- Modify: `src/fabric/mod.rs` (add `pub mod profile;`)

**Interfaces:**
- Produces:
  - `pub struct Artifact { pub name: String, pub cmd: String, pub bundle: Vec<String>, pub format: Option<String>, pub heavy: bool }`
  - `pub struct Profile { pub os: String, pub artifacts: Vec<Artifact> }`
  - `Profile::from_toml_str(s: &str) -> anyhow::Result<Profile>` (validates read-only)
  - `Profile::builtin(os: &str) -> anyhow::Result<Profile>`
  - `Profile::select(&self, bundles: &[String]) -> Vec<&Artifact>` (artifacts in any of `bundles`; if `bundles` is empty, all)
  - `pub fn is_read_only(cmd: &str) -> bool`

- [ ] **Step 1: Write the failing test**

Create `src/fabric/profile.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = r#"
        os = "junos-evo"

        [artifact.version]
        cmd = "show version"
        bundle = ["identity", "support", "forensic"]

        [artifact.config-set]
        cmd = "show configuration | display set"
        bundle = ["config"]
        format = "junos-set"

        [artifact.rsi]
        cmd = "request support information"
        bundle = ["support"]
        heavy = true
    "#;

    #[test]
    fn parses_artifacts_with_defaults() {
        let p = Profile::from_toml_str(P).unwrap();
        assert_eq!(p.os, "junos-evo");
        let v = p.artifacts.iter().find(|a| a.name == "version").unwrap();
        assert_eq!(v.cmd, "show version");
        assert!(!v.heavy);
        assert!(v.format.is_none());
        let rsi = p.artifacts.iter().find(|a| a.name == "rsi").unwrap();
        assert!(rsi.heavy);
    }

    #[test]
    fn select_filters_by_bundle_and_empty_means_all() {
        let p = Profile::from_toml_str(P).unwrap();
        let support: Vec<_> = p.select(&["support".into()]).iter().map(|a| a.name.clone()).collect();
        assert!(support.contains(&"version".to_string()));
        assert!(support.contains(&"rsi".to_string()));
        assert!(!support.contains(&"config-set".to_string()));
        assert_eq!(p.select(&[]).len(), 3); // empty selection = all
    }

    #[test]
    fn rejects_a_config_changing_command() {
        let bad = r#"
            os = "junos"
            [artifact.oops]
            cmd = "set interfaces ge-0/0/0 disable"
            bundle = ["config"]
        "#;
        assert!(Profile::from_toml_str(bad).is_err());
        let reboot = r#"
            os = "junos"
            [artifact.oops]
            cmd = "request system reboot"
        "#;
        assert!(Profile::from_toml_str(reboot).is_err());
    }

    #[test]
    fn read_only_predicate() {
        assert!(is_read_only("show version"));
        assert!(is_read_only("show interfaces terse | no-more"));
        assert!(is_read_only("file show /var/log/messages"));
        assert!(is_read_only("request support information"));
        assert!(!is_read_only("request system reboot"));
        assert!(!is_read_only("set interfaces ge-0/0/0 disable"));
        assert!(!is_read_only("configure"));
    }

    #[test]
    fn builtin_profiles_load_and_validate() {
        assert_eq!(Profile::builtin("junos-evo").unwrap().os, "junos-evo");
        assert_eq!(Profile::builtin("junos").unwrap().os, "junos");
        assert!(Profile::builtin("nonesuch").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p canopy fabric::profile`
Expected: FAIL — `Profile`, `is_read_only` not found.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/fabric/profile.rs`:

```rust
//! Data-driven **vendor profiles**: a TOML file per platform mapping artifact
//! names to the read-only CLI command that collects them, tagged with the
//! export bundles they belong to. Profiles are validated on load so a
//! config-changing command can never enter the collection path.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// One collectable artifact: a named read-only command and its bundle tags.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Artifact id, e.g. `version`, `config-set`. Becomes the store filename stem.
    pub name: String,
    /// The exact read-only CLI command to run.
    pub cmd: String,
    /// Export bundles this artifact belongs to (`identity`, `config`, `support`, ...).
    pub bundle: Vec<String>,
    /// Optional format hint for later redaction (`junos-set`, `junos`, ...).
    pub format: Option<String>,
    /// Heavy commands (e.g. `request support information`) are opt-in and streamed.
    pub heavy: bool,
}

/// A platform's profile: its os id and all its artifacts.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Platform id, e.g. `junos-evo`.
    pub os: String,
    /// All artifacts, in file order.
    pub artifacts: Vec<Artifact>,
}

/// Raw TOML shape before validation/flattening.
#[derive(Debug, Deserialize)]
struct ProfileFile {
    os: String,
    #[serde(default)]
    artifact: BTreeMap<String, ArtifactSpec>,
}

#[derive(Debug, Deserialize)]
struct ArtifactSpec {
    cmd: String,
    #[serde(default)]
    bundle: Vec<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    heavy: bool,
}

/// True if `cmd` is a read-only operational command safe to run during collection:
/// a `show`/`file show`, or a `request … information` (RSI-style). Everything else —
/// `set`, `delete`, `configure`, `request system reboot`, `commit`, … — is rejected.
#[must_use]
pub fn is_read_only(cmd: &str) -> bool {
    let c = cmd.trim();
    c.starts_with("show ")
        || c.starts_with("file show ")
        || (c.starts_with("request ") && c.contains(" information"))
}

impl Profile {
    /// Parse and validate a profile from TOML text.
    ///
    /// # Errors
    /// Fails if the TOML is malformed or any artifact command is not read-only.
    pub fn from_toml_str(s: &str) -> Result<Profile> {
        let raw: ProfileFile = toml::from_str(s).context("parsing profile TOML")?;
        let mut artifacts = Vec::with_capacity(raw.artifact.len());
        for (name, spec) in raw.artifact {
            if !is_read_only(&spec.cmd) {
                bail!("profile {}: artifact '{name}' has a non-read-only command: {}", raw.os, spec.cmd);
            }
            artifacts.push(Artifact {
                name,
                cmd: spec.cmd,
                bundle: spec.bundle,
                format: spec.format,
                heavy: spec.heavy,
            });
        }
        Ok(Profile { os: raw.os, artifacts })
    }

    /// Load one of the built-in profiles compiled into the binary.
    ///
    /// # Errors
    /// Fails for an unknown os, or if a built-in profile fails validation.
    pub fn builtin(os: &str) -> Result<Profile> {
        let text = match os {
            "junos-evo" => include_str!("../../profiles/junos-evo.toml"),
            "junos" => include_str!("../../profiles/junos.toml"),
            other => bail!("no built-in profile for os '{other}'"),
        };
        Self::from_toml_str(text)
    }

    /// The artifacts belonging to any of `bundles`; an empty `bundles` means **all**.
    #[must_use]
    pub fn select(&self, bundles: &[String]) -> Vec<&Artifact> {
        if bundles.is_empty() {
            return self.artifacts.iter().collect();
        }
        self.artifacts
            .iter()
            .filter(|a| a.bundle.iter().any(|b| bundles.contains(b)))
            .collect()
    }
}
```

Add to `src/fabric/mod.rs`:

```rust
pub mod profile;
```

- [ ] **Step 4: Create the built-in profile data files**

Create `profiles/junos-evo.toml` (ACX7024 / PTX10004 — Junos EVO; commands verified against the live ASTRON devices):

```toml
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026  Epsilon Null Operation
# Built-in collection profile for Junos EVO (ACX7024, PTX10004, ...).
os = "junos-evo"

[artifact.version]
cmd = "show version"
bundle = ["identity", "support", "forensic"]

[artifact.uptime]
cmd = "show system uptime"
bundle = ["identity", "forensic"]

[artifact.chassis-hardware]
cmd = "show chassis hardware"
bundle = ["identity", "support", "forensic"]

[artifact.chassis-environment]
cmd = "show chassis environment"
bundle = ["forensic"]

[artifact.config-set]
cmd = "show configuration | display set"
bundle = ["config"]
format = "junos-set"

[artifact.config-native]
cmd = "show configuration"
bundle = ["config"]
format = "junos"

[artifact.lldp]
cmd = "show lldp neighbors"
bundle = ["topology", "support"]

[artifact.interfaces-descriptions]
cmd = "show interfaces descriptions"
bundle = ["topology", "support"]

[artifact.interfaces-terse]
cmd = "show interfaces terse"
bundle = ["state", "support"]

[artifact.interface-errors]
cmd = "show interfaces extensive | match \"error|CRC|FEC|drop|framing\""
bundle = ["forensic"]

[artifact.optics]
cmd = "show interfaces diagnostics optics"
bundle = ["forensic"]

[artifact.arp]
cmd = "show arp no-resolve"
bundle = ["state"]

[artifact.system-alarms]
cmd = "show system alarms"
bundle = ["state", "support", "forensic"]

[artifact.chassis-alarms]
cmd = "show chassis alarms"
bundle = ["state", "support", "forensic"]

[artifact.log-messages]
cmd = "show log messages | last 400"
bundle = ["logs", "support", "forensic"]

[artifact.rsi]
cmd = "request support information"
bundle = ["support"]
heavy = true
```

Create `profiles/junos.toml` (EX4300 — classic Junos):

```toml
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026  Epsilon Null Operation
# Built-in collection profile for classic Junos (EX4300, ...).
os = "junos"

[artifact.version]
cmd = "show version"
bundle = ["identity", "support", "forensic"]

[artifact.uptime]
cmd = "show system uptime"
bundle = ["identity", "forensic"]

[artifact.chassis-hardware]
cmd = "show chassis hardware"
bundle = ["identity", "support", "forensic"]

[artifact.chassis-environment]
cmd = "show chassis environment"
bundle = ["forensic"]

[artifact.config-set]
cmd = "show configuration | display set"
bundle = ["config"]
format = "junos-set"

[artifact.config-native]
cmd = "show configuration"
bundle = ["config"]
format = "junos"

[artifact.lldp]
cmd = "show lldp neighbors"
bundle = ["topology", "support"]

[artifact.interfaces-descriptions]
cmd = "show interfaces descriptions"
bundle = ["topology", "support"]

[artifact.interfaces-terse]
cmd = "show interfaces terse"
bundle = ["state", "support"]

[artifact.interface-errors]
cmd = "show interfaces extensive | match \"error|CRC|drop|framing\""
bundle = ["forensic"]

[artifact.optics]
cmd = "show interfaces diagnostics optics"
bundle = ["forensic"]

[artifact.ethernet-switching-table]
cmd = "show ethernet-switching table"
bundle = ["state"]

[artifact.system-alarms]
cmd = "show system alarms"
bundle = ["state", "support", "forensic"]

[artifact.chassis-alarms]
cmd = "show chassis alarms"
bundle = ["state", "support", "forensic"]

[artifact.log-messages]
cmd = "show log messages | last 400"
bundle = ["logs", "support", "forensic"]

[artifact.rsi]
cmd = "request support information"
bundle = ["support"]
heavy = true
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p canopy fabric::profile`
Expected: PASS (5 tests). Then `cargo build 2>&1 | grep -c warning` → `0`.

- [ ] **Step 6: Commit**

```bash
git add src/fabric/mod.rs src/fabric/profile.rs profiles/junos-evo.toml profiles/junos.toml
git commit -m "fabric: data-driven vendor profiles (junos-evo, junos) with read-only validation"
```

---

### Task 3: Snapshot store + manifest

**Files:**
- Create: `src/fabric/store.rs`
- Modify: `src/fabric/mod.rs` (add `pub mod store;`)
- Modify: `Cargo.toml` (add `sha2`, `chrono`; dev-dep `tempfile`)

**Interfaces:**
- Consumes: `chrono::{DateTime, Utc}`.
- Produces:
  - `pub struct ArtifactRecord { pub name: String, pub command: String, pub sha256: String, pub bytes: u64, pub exit: i32 }`
  - `pub struct Manifest { pub device: String, pub host: String, pub via: String, pub user: String, pub canopy_version: String, pub read_only: bool, pub collected_at: String, pub device_clock: Option<String>, pub clock_skew_secs: Option<i64>, pub artifacts: Vec<ArtifactRecord> }`
  - `pub fn sha256_hex(bytes: &[u8]) -> String`
  - `pub struct Store { root: PathBuf }` with `Store::new(root: PathBuf) -> Store`
  - `Store::begin(&self, site: &str, device: &str, at: DateTime<Utc>) -> anyhow::Result<SnapshotWriter>`
  - `pub struct SnapshotWriter { … }` with:
    - `write_artifact(&mut self, name: &str, command: &str, body: &str, exit: i32) -> anyhow::Result<()>`
    - `finish(self, manifest_head: ManifestHead) -> anyhow::Result<PathBuf>` (writes `manifest.json`, updates `latest`, returns snapshot dir)
  - `pub struct ManifestHead { pub device: String, pub host: String, pub via: String, pub user: String, pub device_clock: Option<String>, pub clock_skew_secs: Option<i64>, pub collected_at: String }`

- [ ] **Step 1: Add dependencies**

In `Cargo.toml` under `[dependencies]` add:

```toml
# Artifact checksums for tamper-evident manifests.
sha2       = "0.10"
# UTC timestamps for snapshot ids and device-clock skew.
chrono     = { version = "0.4", default-features = false, features = ["clock", "std"] }
```

Add a `[dev-dependencies]` section (or extend it) with:

```toml
[dev-dependencies]
tempfile = "3"
```

Run: `cargo build` — Expected: builds, pulls the new crates.

- [ ] **Step 2: Write the failing test**

Create `src/fabric/store.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn head() -> ManifestHead {
        ManifestHead {
            device: "acx-a2-0".into(),
            host: "10.155.251.23".into(),
            via: "".into(),
            user: "nagtegaal".into(),
            device_clock: Some("2026-07-07 11:49:07".into()),
            clock_skew_secs: Some(0),
            collected_at: "2026-07-07T11:49:10Z".into(),
        }
    }

    #[test]
    fn sha256_is_stable_and_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn snapshot_writes_files_manifest_and_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().to_path_buf());
        let at = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 11, 49, 10).unwrap();

        let mut snap = store.begin("astron", "acx-a2-0", at).unwrap();
        snap.write_artifact("version", "show version", "Model: ACX7024\n", 0).unwrap();
        snap.write_artifact("lldp", "show lldp neighbors", "", 0).unwrap();
        let dir = snap.finish(head()).unwrap();

        // artifact files exist with expected content
        assert_eq!(std::fs::read_to_string(dir.join("version.txt")).unwrap(), "Model: ACX7024\n");
        assert!(dir.join("lldp.txt").exists());

        // manifest parses and records both artifacts + checksums
        let mtext = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let m: Manifest = serde_json::from_str(&mtext).unwrap();
        assert_eq!(m.device, "acx-a2-0");
        assert!(m.read_only);
        assert_eq!(m.artifacts.len(), 2);
        let v = m.artifacts.iter().find(|a| a.name == "version").unwrap();
        assert_eq!(v.sha256, sha256_hex(b"Model: ACX7024\n"));
        assert_eq!(v.bytes, 15);

        // `latest` points at this snapshot
        let latest = tmp.path().join("astron").join("acx-a2-0").join("latest");
        assert_eq!(std::fs::read_link(&latest).unwrap(), dir);
    }

    #[test]
    fn latest_advances_to_the_newest_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().to_path_buf());
        let t1 = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 10, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 12, 0, 0).unwrap();

        let mut s1 = store.begin("astron", "acx-a2-0", t1).unwrap();
        s1.write_artifact("version", "show version", "one\n", 0).unwrap();
        s1.finish(head()).unwrap();

        let mut s2 = store.begin("astron", "acx-a2-0", t2).unwrap();
        s2.write_artifact("version", "show version", "two\n", 0).unwrap();
        let dir2 = s2.finish(head()).unwrap();

        let latest = tmp.path().join("astron").join("acx-a2-0").join("latest");
        assert_eq!(std::fs::read_link(&latest).unwrap(), dir2);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p canopy fabric::store`
Expected: FAIL — `Store`, `Manifest`, `sha256_hex` not found.

- [ ] **Step 4: Write the minimal implementation**

Prepend to `src/fabric/store.rs`:

```rust
//! The on-disk **snapshot store**: each collection of a device is written to
//! `<root>/<site>/<device>/<UTC-timestamp>/`, with a `manifest.json` recording
//! provenance and a sha256 per artifact, and a `latest` symlink advanced to the
//! newest snapshot. Versioned so degradation can be shown over time.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One artifact's record in a snapshot manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub name: String,
    pub command: String,
    pub sha256: String,
    pub bytes: u64,
    pub exit: i32,
}

/// Provenance for a snapshot (everything but the per-artifact records, which the
/// writer fills in as it goes).
#[derive(Debug, Clone)]
pub struct ManifestHead {
    pub device: String,
    pub host: String,
    pub via: String,
    pub user: String,
    pub device_clock: Option<String>,
    pub clock_skew_secs: Option<i64>,
    pub collected_at: String,
}

/// The full snapshot manifest written as `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub device: String,
    pub host: String,
    pub via: String,
    pub user: String,
    pub canopy_version: String,
    pub read_only: bool,
    pub collected_at: String,
    pub device_clock: Option<String>,
    pub clock_skew_secs: Option<i64>,
    pub artifacts: Vec<ArtifactRecord>,
}

/// Lowercase hex sha256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A versioned snapshot store rooted at `root`.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// A store rooted at `root` (e.g. `~/.local/share/canopy/fabric`).
    #[must_use]
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    /// The default store root: `$XDG_DATA_HOME/canopy/fabric` or `~/.local/share/...`.
    #[must_use]
    pub fn default_root() -> PathBuf {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(x).join("canopy").join("fabric");
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".local/share/canopy/fabric")
    }

    /// Start a new snapshot for `device` under `site`, timestamped `at`.
    ///
    /// # Errors
    /// Fails if the snapshot directory cannot be created.
    pub fn begin(&self, site: &str, device: &str, at: DateTime<Utc>) -> Result<SnapshotWriter> {
        let stamp = at.format("%Y-%m-%dT%H-%M-%SZ").to_string();
        let device_dir = self.root.join(site).join(device);
        let dir = device_dir.join(&stamp);
        fs::create_dir_all(&dir).with_context(|| format!("creating snapshot dir {}", dir.display()))?;
        Ok(SnapshotWriter { device_dir, dir, records: Vec::new() })
    }
}

/// An in-progress snapshot: write artifacts, then `finish` to seal the manifest.
#[derive(Debug)]
pub struct SnapshotWriter {
    device_dir: PathBuf,
    dir: PathBuf,
    records: Vec<ArtifactRecord>,
}

impl SnapshotWriter {
    /// Write one artifact's `body` to `<name>.txt` and record its checksum.
    ///
    /// # Errors
    /// Fails if the file cannot be written.
    pub fn write_artifact(&mut self, name: &str, command: &str, body: &str, exit: i32) -> Result<()> {
        let path = self.dir.join(format!("{name}.txt"));
        fs::write(&path, body).with_context(|| format!("writing artifact {}", path.display()))?;
        self.records.push(ArtifactRecord {
            name: name.to_string(),
            command: command.to_string(),
            sha256: sha256_hex(body.as_bytes()),
            bytes: body.as_bytes().len() as u64,
            exit,
        });
        Ok(())
    }

    /// Seal the snapshot: write `manifest.json`, advance `latest`, return the dir.
    ///
    /// # Errors
    /// Fails if the manifest cannot be written or the `latest` link updated.
    pub fn finish(self, head: ManifestHead) -> Result<PathBuf> {
        let manifest = Manifest {
            device: head.device,
            host: head.host,
            via: head.via,
            user: head.user,
            canopy_version: env!("CARGO_PKG_VERSION").to_string(),
            read_only: true,
            collected_at: head.collected_at,
            device_clock: head.device_clock,
            clock_skew_secs: head.clock_skew_secs,
            artifacts: self.records,
        };
        let json = serde_json::to_string_pretty(&manifest).context("serializing manifest")?;
        fs::write(self.dir.join("manifest.json"), json).context("writing manifest.json")?;
        update_latest(&self.device_dir, &self.dir)?;
        Ok(self.dir)
    }
}

/// Point `<device_dir>/latest` at `target` (replacing any existing link).
fn update_latest(device_dir: &Path, target: &Path) -> Result<()> {
    let link = device_dir.join("latest");
    if link.symlink_metadata().is_ok() {
        fs::remove_file(&link).with_context(|| format!("removing old latest link {}", link.display()))?;
    }
    std::os::unix::fs::symlink(target, &link)
        .with_context(|| format!("linking latest -> {}", target.display()))?;
    Ok(())
}
```

Add to `src/fabric/mod.rs`:

```rust
pub mod store;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p canopy fabric::store`
Expected: PASS (3 tests). Then `cargo build 2>&1 | grep -c warning` → `0`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/fabric/mod.rs src/fabric/store.rs
git commit -m "fabric: versioned snapshot store with provenance + sha256 manifest"
```

---

### Task 4: Collect orchestrator

**Files:**
- Create: `src/fabric/collect.rs`
- Modify: `src/fabric/mod.rs` (add `pub mod collect;`)

**Interfaces:**
- Consumes: `Device` (Task 1), `Profile`/`Artifact` (Task 2), `Store`/`ManifestHead`/`Manifest` (Task 3), `chrono::{DateTime, Utc}`.
- Produces:
  - `pub trait CommandRunner { fn exec(&self, cmd: &str) -> anyhow::Result<String>; }`
  - `impl CommandRunner for crate::sources::vantage::Vantage`
  - `pub fn detect_os(version_output: &str) -> &'static str`
  - `pub fn parse_device_clock(text: &str) -> Option<String>` (the `Current time:` line, `YYYY-MM-DD HH:MM:SS`)
  - `pub fn collect(runner: &dyn CommandRunner, device: &Device, profile: &Profile, bundles: &[String], store: &Store, site: &str, site_jump: &str, now: DateTime<Utc>) -> anyhow::Result<PathBuf>`

- [ ] **Step 1: Write the failing test**

Create `src/fabric/collect.rs`:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fabric::inventory::Inventory;
    use crate::fabric::profile::Profile;
    use crate::fabric::store::{Manifest, Store};
    use chrono::TimeZone;
    use std::collections::HashMap;

    /// A fake runner returning canned output per command — no ssh, no device.
    struct FakeRunner(HashMap<String, String>);
    impl CommandRunner for FakeRunner {
        fn exec(&self, cmd: &str) -> anyhow::Result<String> {
            self.0
                .get(cmd)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("fake has no output for: {cmd}"))
        }
    }

    fn profile() -> Profile {
        Profile::from_toml_str(
            r#"
            os = "junos-evo"
            [artifact.version]
            cmd = "show version"
            bundle = ["identity"]
            [artifact.uptime]
            cmd = "show system uptime"
            bundle = ["identity"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn detects_evo_vs_classic() {
        assert_eq!(detect_os("Model: ACX7024\nJunos: 22.3R1.9-EVO\n"), "junos-evo");
        assert_eq!(detect_os("Model: ex4300-48p\nJunos: 18.1R3-S8.3\n"), "junos");
    }

    #[test]
    fn parses_current_time_line() {
        let up = "Current time: 2026-07-07 11:49:07 UTC\nSystem booted: ...\n";
        assert_eq!(parse_device_clock(up).as_deref(), Some("2026-07-07 11:49:07"));
        assert!(parse_device_clock("no time here").is_none());
    }

    #[test]
    fn collect_runs_selected_artifacts_and_writes_snapshot() {
        let inv = Inventory::from_toml_str(
            r#"[[device]]
               name = "acx-a2-0"
               host = "10.155.251.23"
               user = "nagtegaal""#,
        )
        .unwrap();
        let dev = inv.get("acx-a2-0").unwrap();

        let mut outputs = HashMap::new();
        outputs.insert("show version".to_string(), "Model: ACX7024\nJunos: 22.3R1.9-EVO\n".to_string());
        outputs.insert("show system uptime".to_string(), "Current time: 2026-07-07 11:49:07 UTC\n".to_string());
        let runner = FakeRunner(outputs);

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path().to_path_buf());
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 7, 11, 49, 10).unwrap();

        let dir = collect(&runner, dev, &profile(), &["identity".into()], &store, "astron", "", now).unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("version.txt")).unwrap(), "Model: ACX7024\nJunos: 22.3R1.9-EVO\n");
        let m: Manifest = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.device, "acx-a2-0");
        assert_eq!(m.user, "nagtegaal");
        assert!(m.read_only);
        assert_eq!(m.artifacts.len(), 2);
        // device clock parsed from uptime; skew ~3s behind `now`
        assert_eq!(m.device_clock.as_deref(), Some("2026-07-07 11:49:07"));
        assert_eq!(m.clock_skew_secs, Some(3));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p canopy fabric::collect`
Expected: FAIL — `CommandRunner`, `collect`, `detect_os` not found.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/fabric/collect.rs`:

```rust
//! The **collect** orchestrator: run a device's selected read-only artifacts over
//! an injected [`CommandRunner`] and write them as one versioned snapshot. The
//! runner seam keeps the whole pipeline unit-testable with a fake; production uses
//! a [`Vantage`](crate::sources::vantage::Vantage) that shells out to `ssh`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};

use crate::fabric::inventory::Device;
use crate::fabric::profile::Profile;
use crate::fabric::store::{ManifestHead, Store};
use crate::sources::vantage::Vantage;

/// The one I/O seam: run a command on a device and return its stdout.
pub trait CommandRunner {
    /// Run `cmd` and return stdout.
    ///
    /// # Errors
    /// Fails if the command cannot be run or exits non-zero.
    fn exec(&self, cmd: &str) -> Result<String>;
}

impl CommandRunner for Vantage {
    fn exec(&self, cmd: &str) -> Result<String> {
        self.run(cmd)
    }
}

/// Best-effort platform id from `show version`: EVO builds say `-EVO`.
#[must_use]
pub fn detect_os(version_output: &str) -> &'static str {
    if version_output.contains("EVO") {
        "junos-evo"
    } else {
        "junos"
    }
}

/// The device's wall clock from a `show system uptime` (`Current time:`) line, as
/// `YYYY-MM-DD HH:MM:SS` (timezone suffix dropped).
#[must_use]
pub fn parse_device_clock(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("Current time:") {
            let t = rest.trim();
            // keep the leading "YYYY-MM-DD HH:MM:SS", drop any trailing " UTC"
            let cleaned: String = t.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Skew in seconds between `now` and the parsed device clock (positive = device behind).
fn clock_skew(now: DateTime<Utc>, device_clock: &str) -> Option<i64> {
    let dt = NaiveDateTime::parse_from_str(device_clock, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(now.naive_utc().signed_duration_since(dt).num_seconds())
}

/// Collect `device`'s artifacts in `bundles` into a new snapshot under `site`.
///
/// # Errors
/// Fails if a command errors or the snapshot cannot be written.
#[allow(clippy::too_many_arguments)]
pub fn collect(
    runner: &dyn CommandRunner,
    device: &Device,
    profile: &Profile,
    bundles: &[String],
    store: &Store,
    site: &str,
    site_jump: &str,
    now: DateTime<Utc>,
) -> Result<PathBuf> {
    let selected = profile.select(bundles);
    let mut snap = store.begin(site, &device.name, now)?;

    let mut device_clock: Option<String> = None;
    for art in &selected {
        let body = runner
            .exec(&art.cmd)
            .with_context(|| format!("collecting {} on {}", art.name, device.name))?;
        if device_clock.is_none() {
            device_clock = parse_device_clock(&body);
        }
        snap.write_artifact(&art.name, &art.cmd, &body, 0)?;
    }

    let skew = device_clock.as_deref().and_then(|c| clock_skew(now, c));
    let via = device.vantage(site_jump).jump;
    let head = ManifestHead {
        device: device.name.clone(),
        host: device.host.clone(),
        via,
        user: device.user.clone().unwrap_or_default(),
        device_clock,
        clock_skew_secs: skew,
        collected_at: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    };
    snap.finish(head)
}
```

Add to `src/fabric/mod.rs`:

```rust
pub mod collect;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p canopy fabric::collect`
Expected: PASS (3 tests). Then `cargo build 2>&1 | grep -c warning` → `0`.

- [ ] **Step 5: Commit**

```bash
git add src/fabric/mod.rs src/fabric/collect.rs
git commit -m "fabric: collect orchestrator over an injected CommandRunner (testable headless)"
```

---

### Task 5: `canopy fabric collect` CLI

**Files:**
- Modify: `src/main.rs` (add the `fabric` subcommand + handler)

**Interfaces:**
- Consumes: everything above; `clap` (already a dependency, derive feature).
- Produces: a `canopy fabric collect --device <name> [--bundle <b>]… [--site-file <path>] [--jump <chain>]` command that loads the inventory, resolves the profile (device `os` or auto-detect via a first `show version`), builds a `Vantage`, collects, and prints the snapshot path + a one-line summary.

> Note on wiring: `src/main.rs` already parses args with clap. Read the existing
> `Cli`/`Commands` enum before editing and add a `Fabric` variant in the same style.
> The block below shows the self-contained handler; adapt the enum wiring to match
> the file's existing derive structure.

- [ ] **Step 1: Add the subcommand types and handler to `src/main.rs`**

Add these types near the other clap types:

```rust
use clap::{Args, Subcommand};

/// `canopy fabric …` — read-only collection from network devices.
#[derive(Debug, Args)]
struct FabricArgs {
    #[command(subcommand)]
    cmd: FabricCmd,
}

#[derive(Debug, Subcommand)]
enum FabricCmd {
    /// Collect a device's artifacts into a new snapshot.
    Collect(FabricCollectArgs),
}

#[derive(Debug, Args)]
struct FabricCollectArgs {
    /// Device name as it appears in the `[[device]]` inventory.
    #[arg(long)]
    device: String,
    /// Artifact bundle(s) to collect (repeatable); omitted = all artifacts.
    #[arg(long = "bundle")]
    bundles: Vec<String>,
    /// Site TOML file holding the `[[device]]` inventory.
    #[arg(long, default_value = "~/.config/canopy/conf.d/astron.toml")]
    site_file: String,
    /// Site name (store subdirectory).
    #[arg(long, default_value = "astron")]
    site: String,
    /// Site-wide ProxyJump chain used when a device sets none.
    #[arg(long, default_value = "")]
    jump: String,
}
```

Add the handler function:

```rust
/// Handle `canopy fabric collect`.
fn run_fabric_collect(a: &FabricCollectArgs) -> anyhow::Result<()> {
    use fabric::collect::{collect, detect_os, CommandRunner};
    use fabric::inventory::Inventory;
    use fabric::profile::Profile;
    use fabric::store::Store;

    let path = expand_tilde(&a.site_file);
    let inv = Inventory::load(std::path::Path::new(&path))?;
    let device = inv
        .get(&a.device)
        .ok_or_else(|| anyhow::anyhow!("device '{}' not in inventory {}", a.device, path))?;

    let vantage = device.vantage(&a.jump);

    // Resolve the profile: configured os, else detect from a first `show version`.
    let os = match &device.os {
        Some(os) => os.clone(),
        None => {
            let ver = vantage.exec("show version")?;
            detect_os(&ver).to_string()
        }
    };
    let profile = Profile::builtin(&os)?;

    let store = Store::new(Store::default_root());
    let now = chrono::Utc::now();
    let dir = collect(&vantage, device, &profile, &a.bundles, &store, &a.site, &a.jump, now)?;

    let n = profile.select(&a.bundles).len();
    println!("collected {n} artifact(s) from {} -> {}", device.name, dir.display());
    Ok(())
}

/// Expand a leading `~/` to `$HOME`.
fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}
```

Wire the `Fabric(FabricArgs)` variant into the existing top-level command enum and dispatch:

```rust
        Commands::Fabric(f) => match &f.cmd {
            FabricCmd::Collect(a) => run_fabric_collect(a)?,
        },
```

- [ ] **Step 2: Write the failing test**

Add to `src/main.rs` a test module (or extend the existing one):

```rust
#[cfg(test)]
mod fabric_cli_tests {
    use super::*;

    #[test]
    fn expands_leading_tilde() {
        std::env::set_var("HOME", "/home/roland");
        assert_eq!(expand_tilde("~/x/y.toml"), "/home/roland/x/y.toml");
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
    }
}
```

- [ ] **Step 3: Run test to verify it fails, then build**

Run: `cargo test -p canopy fabric_cli_tests`
Expected: FAIL if `expand_tilde` not yet added; once Step 1 is in, PASS.
Then: `cargo build 2>&1 | grep -c warning` → `0`, and `cargo run -- fabric collect --help` prints the subcommand help.

- [ ] **Step 4: Live smoke test (manual, against a reachable device)**

Ensure the site file has a `[[device]]` for `acx-a2-0` (host `10.155.251.23`, user `nagtegaal`). Then:

Run: `cargo run -- fabric collect --device acx-a2-0 --bundle identity`
Expected: prints `collected N artifact(s) from acx-a2-0 -> ~/.local/share/canopy/fabric/astron/acx-a2-0/<ts>`, and that dir contains `version.txt`, `uptime.txt`, `chassis-hardware.txt`, and a `manifest.json` with `read_only: true` and a plausible `clock_skew_secs`.

Verify integrity: `cd <snapshot dir> && sha256sum version.txt` matches the `sha256` for `version` in `manifest.json`.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "fabric: canopy fabric collect CLI (inventory -> profile -> snapshot)"
```

---

## Self-Review

**Spec coverage (against `2026-07-07-fabric-collection-export-design.md`):**
- Inventory & access (`[[device]]`, reuse Vantage, per-device jump) → Task 1. ✓
- Data-driven vendor profiles + read-only validation → Task 2. ✓
- Versioned store + provenance/checksum manifest + `latest` → Task 3. ✓
- Collect orchestration + vendor auto-detect + device-clock skew → Task 4. ✓
- CLI-first entry point (`canopy fabric collect`), build steps 1–3 → Task 5. ✓
- **Deferred to follow-on plans (as scoped):** redaction (spec §6), export/presets/tarball (spec §7), TUI view (spec §8). Noted in the header. ✓

**Placeholder scan:** No TBD/TODO; every code step carries complete code. The one non-literal item — the top-level clap enum wiring in Task 5 — is explicitly flagged because it must match `main.rs`'s existing structure, and the handler code it dispatches to is given in full.

**Type consistency:** `CommandRunner::exec` (Task 4) is the same name used by the `FakeRunner` test and the `Vantage` impl and the CLI (`vantage.exec("show version")`). `ManifestHead`/`Manifest`/`ArtifactRecord`/`Store::begin`/`SnapshotWriter::{write_artifact,finish}` names match between Task 3 and Task 4. `Profile::{from_toml_str,builtin,select}`, `Device::{vantage,ssh_host}`, `Inventory::{from_toml_str,load,get}` are consistent across tasks. `sha256_hex` used identically in Tasks 3 and its tests.

**Note on `Store::default_root`:** uses `unwrap_or_else` on `HOME` (startup/`main`-adjacent path resolution), consistent with the "no unwrap outside main/startup" rule; it never panics.
