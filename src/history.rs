// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Git-backed history of the estate mirror (roadmap P15): each sync commits the per-zone snapshots,
//! so `git log` / `git diff` under the mirror dir *is* the estate's changelog, and `--since <rev>`
//! surfaces the reconcile-relevant changes — a host **appeared**, **vanished**, or was **renamed**
//! in DNS (the PTR level the mirror records).
//!
//! The diff is **pure** (and tested); the git plumbing is **best-effort** — no `git` on the path,
//! or a first run with no repo yet, simply means no history, never a failed launch. Read-only
//! against the world: the only repo canopy ever writes is its own mirror, never a DNS master.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::Path;
use std::process::Command;

use crate::cache::Snapshot;
use crate::reconcile::AddressFacts;

/// What changed between two syncs, at the DNS-PTR level the mirror records.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EstateDelta {
    /// Addresses that gained a name — a host appeared in DNS.
    pub appeared: Vec<(IpAddr, String)>,
    /// Addresses whose name went away — a host vanished from DNS.
    pub vanished: Vec<(IpAddr, String)>,
    /// Addresses whose name changed, as `(addr, old, new)`.
    pub renamed: Vec<(IpAddr, String, String)>,
}

impl EstateDelta {
    /// Nothing changed between the two revisions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.appeared.is_empty() && self.vanished.is_empty() && self.renamed.is_empty()
    }

    /// A human report: `+` appeared, `~` renamed, `-` vanished — address-sorted within each group.
    #[must_use]
    pub fn render(&self) -> String {
        if self.is_empty() {
            return "no changes\n".to_string();
        }
        let mut s = String::new();
        for (a, n) in &self.appeared {
            s.push_str(&format!("+ {a}  {n}\n"));
        }
        for (a, o, n) in &self.renamed {
            s.push_str(&format!("~ {a}  {o} → {n}\n"));
        }
        for (a, n) in &self.vanished {
            s.push_str(&format!("- {a}  {n}\n"));
        }
        s
    }
}

/// The name a fact carries — NetBox's DNS name if present, else the PTR — trailing dot stripped.
fn name_of(f: &AddressFacts) -> Option<String> {
    f.netbox
        .as_ref()
        .and_then(|n| n.dns_name.clone())
        .or_else(|| f.ptr.clone())
        .map(|s| s.trim_end_matches('.').to_string())
}

/// Diff two fact sets into an [`EstateDelta`]. Pure and deterministic (address-ordered), so the
/// same pair always yields identical output — the property the P15 tests pin.
#[must_use]
pub fn diff_facts(old: &[AddressFacts], new: &[AddressFacts]) -> EstateDelta {
    let om: BTreeMap<IpAddr, String> = old.iter().filter_map(|f| Some((f.addr, name_of(f)?))).collect();
    let nm: BTreeMap<IpAddr, String> = new.iter().filter_map(|f| Some((f.addr, name_of(f)?))).collect();
    let mut d = EstateDelta::default();
    for (addr, name) in &nm {
        match om.get(addr) {
            None => d.appeared.push((*addr, name.clone())),
            Some(prev) if prev != name => d.renamed.push((*addr, prev.clone(), name.clone())),
            _ => {}
        }
    }
    for (addr, name) in &om {
        if !nm.contains_key(addr) {
            d.vanished.push((*addr, name.clone()));
        }
    }
    d
}

// ── git plumbing (best-effort) ──────────────────────────────────────────────────────────────────

/// Run `git -C <dir> <args…>`, returning stdout on success, `None` on any failure (git missing,
/// non-zero exit, bad UTF-8). Never panics; the caller treats `None` as "no history available".
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Commit the current mirror as one sync. Initialises the repo on first use, stages everything, and
/// commits with an identity passed inline (so it works without global git config). Best-effort: an
/// empty commit (nothing changed) or a missing `git` is silently a no-op.
pub fn commit_sync(dir: &Path, site: &str, when: &str) {
    if !dir.join(".git").exists() {
        let _ = git(dir, &["init", "-q"]);
    }
    let _ = git(dir, &["add", "-A"]);
    let msg = format!("synced {site} @ {when}");
    let _ = git(dir, &["-c", "user.name=canopy", "-c", "user.email=canopy@localhost", "commit", "-q", "-m", &msg]);
}

/// The facts recorded at a git revision, unioned across every zone snapshot at that point. Empty
/// when the rev or repo can't be read (best-effort).
#[must_use]
pub fn facts_at(dir: &Path, rev: &str) -> Vec<AddressFacts> {
    let mut out = Vec::new();
    let Some(files) = git(dir, &["ls-tree", "-r", "--name-only", rev]) else { return out };
    for f in files.lines().filter(|f| f.ends_with(".txt")) {
        if let Some(text) = git(dir, &["show", &format!("{rev}:{f}")]) {
            if let Some(snap) = Snapshot::from_text(&text) {
                out.extend(snap.facts);
            }
        }
    }
    out
}

/// Resolve a user-supplied `<since>` to a concrete commit: a real revision as-is (`HEAD~2`, a hash,
/// a tag), else the last commit before a date/time expression (`2 weeks ago`, `2026-06-01`).
/// `None` when neither resolves.
#[must_use]
pub fn resolve_rev(dir: &Path, since: &str) -> Option<String> {
    // A concrete revision first (hash, HEAD~2, tag).
    if let Some(h) = git(dir, &["rev-parse", "--verify", "-q", since]) {
        let h = h.trim().to_string();
        if !h.is_empty() {
            return Some(h);
        }
    }
    // Otherwise a time expression → the last commit at or before it.
    let h = git(dir, &["rev-list", "-1", &format!("--before={since}"), "HEAD"])?;
    let h = h.trim().to_string();
    (!h.is_empty()).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(addr: &str, name: &str) -> AddressFacts {
        AddressFacts { addr: addr.parse().unwrap(), netbox: None, ptr: Some(format!("{name}.")), live: false }
    }

    #[test]
    fn diff_surfaces_appeared_vanished_renamed() {
        let old = vec![fact("10.87.3.5", "old"), fact("10.87.3.6", "gone"), fact("10.87.3.7", "steady")];
        let new = vec![fact("10.87.3.5", "renamed"), fact("10.87.3.7", "steady"), fact("10.87.3.9", "fresh")];
        let d = diff_facts(&old, &new);
        assert_eq!(d.appeared, vec![("10.87.3.9".parse().unwrap(), "fresh".into())]);
        assert_eq!(d.vanished, vec![("10.87.3.6".parse().unwrap(), "gone".into())]);
        assert_eq!(d.renamed, vec![("10.87.3.5".parse().unwrap(), "old".into(), "renamed".into())]);
    }

    #[test]
    fn diff_is_deterministic_and_empty_when_unchanged() {
        let facts = vec![fact("10.0.0.2", "b"), fact("10.0.0.1", "a")];
        assert!(diff_facts(&facts, &facts).is_empty(), "same facts → no changes");
        // Address-ordered regardless of input order → identical render.
        let shuffled = vec![fact("10.0.0.1", "a"), fact("10.0.0.2", "b")];
        assert_eq!(diff_facts(&[], &facts).render(), diff_facts(&[], &shuffled).render());
    }

    #[test]
    fn git_round_trips_a_sync_history() {
        // Skip cleanly where git isn't available (some CI sandboxes).
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = crate::cache::Store::open(dir.path()).unwrap();

        // Sync 1: one host.
        store.save(&Snapshot { zone: "z".into(), view: "v".into(), serial: 1, synced: 1, facts: vec![fact("10.0.0.1", "before")] }).unwrap();
        commit_sync(dir.path(), "astron", "t1");
        // Sync 2: renamed.
        store.save(&Snapshot { zone: "z".into(), view: "v".into(), serial: 2, synced: 2, facts: vec![fact("10.0.0.1", "after")] }).unwrap();
        commit_sync(dir.path(), "astron", "t2");

        let old = facts_at(dir.path(), "HEAD~1");
        let new = store.load_all().into_iter().flat_map(|s| s.facts).collect::<Vec<_>>();
        assert_eq!(old.len(), 1, "the previous revision is readable from git");
        let d = diff_facts(&old, &new);
        assert_eq!(d.renamed, vec![("10.0.0.1".parse().unwrap(), "before".into(), "after".into())]);
    }
}
