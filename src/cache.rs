// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! On-disk cache of the surveyed estate, so a second sweep is cheap (roadmap P13). Per
//! **(zone, view)** we keep the last SOA **serial**, when we synced, and the facts that transfer
//! yielded; on launch a cheap `dig SOA` tells us whether the zone changed *at all* before we pay
//! for an AXFR.
//!
//! Keyed by `(zone, view)` — never the zone alone — because the estate is **split-horizon**: two
//! different `10/8` views (lcs020 vs ntserver1) share a zone name but hold different data, and a
//! zone-only key would conflate them.
//!
//! Stored as **normalised, one-record-per-line text** (a header line, then facts sorted by
//! address), so P15 can drop it under git and `git diff` reads as the estate's drift. This module
//! is I/O-light and pure: it (de)serialises snapshots and answers the freshness question; the file
//! reads/writes and the `dig SOA` probe live in the caller.
//!
//! Forward-declared P13 infrastructure: the store + freshness check are built and tested here; the
//! launch-time wiring (probe SOA → skip fresh zones → load from cache) lands next, at which point
//! these become used. The `allow` keeps the build warning-free until then.
#![allow(dead_code)]

use std::net::IpAddr;

use crate::reconcile::{AddressFacts, NetBoxRecord};

/// One cached zone-view: the serial we last saw, the unix time we synced, and the facts that the
/// transfer produced. The `(zone, view)` pair is the cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The zone apex (e.g. `nfra.nl`, `10.in-addr.arpa`).
    pub zone: String,
    /// The view that mastered it — the server/vantage — so split-horizon zones stay distinct.
    pub view: String,
    /// The SOA serial at sync time; an unchanged serial means the zone is unchanged.
    pub serial: u64,
    /// Unix seconds when this snapshot was taken (stamped by the caller, so this stays pure).
    pub synced: u64,
    /// The facts the transfer produced, in address order.
    pub facts: Vec<AddressFacts>,
}

/// Whether a cached serial is still current given a freshly-probed one — the cheap check that
/// decides "skip the AXFR". A missing cache (`None`) is never fresh.
#[must_use]
pub fn is_fresh(cached: Option<u64>, probed: u64) -> bool {
    cached == Some(probed)
}

impl Snapshot {
    /// Serialise to normalised text: one header line, then one sorted fact per line. Deterministic
    /// (same snapshot → identical bytes), so a git commit of it diffs cleanly.
    ///
    /// Fact line: `addr \t in_netbox(0|1) \t netbox_name|- \t ptr|- \t live(0|1)`.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = format!("# zone {} view {} serial {} synced {}\n", self.zone, self.view, self.serial, self.synced);
        let mut facts = self.facts.clone();
        facts.sort_by_key(|f| f.addr);
        for f in &facts {
            let (in_nb, name) = match &f.netbox {
                Some(r) => (1, r.dns_name.as_deref().unwrap_or("-")),
                None => (0, "-"),
            };
            let ptr = f.ptr.as_deref().unwrap_or("-");
            out.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", f.addr, in_nb, name, ptr, u8::from(f.live)));
        }
        out
    }

    /// Parse a [`Snapshot`] from [`to_text`](Snapshot::to_text) output. `None` if the header line
    /// is missing or malformed; a malformed fact line is skipped rather than fatal.
    #[must_use]
    pub fn from_text(s: &str) -> Option<Snapshot> {
        let mut lines = s.lines();
        let header: Vec<&str> = lines.next()?.split_whitespace().collect();
        // `# zone <z> view <v> serial <n> synced <t>`
        let field = |key: &str| header.iter().position(|&t| t == key).and_then(|i| header.get(i + 1)).copied();
        let zone = field("zone")?.to_string();
        let view = field("view")?.to_string();
        let serial = field("serial")?.parse().ok()?;
        let synced = field("synced")?.parse().ok()?;

        let mut facts = Vec::new();
        for line in lines {
            let c: Vec<&str> = line.split('\t').collect();
            let [addr, in_nb, name, ptr, live] = c[..] else { continue };
            let Ok(addr) = addr.parse::<IpAddr>() else { continue };
            let netbox = (in_nb == "1").then(|| NetBoxRecord {
                dns_name: (name != "-").then(|| name.to_string()),
            });
            facts.push(AddressFacts {
                addr,
                netbox,
                ptr: (ptr != "-").then(|| ptr.to_string()),
                live: live == "1",
            });
        }
        Some(Snapshot { zone, view, serial, synced, facts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(addr: &str, nb: Option<Option<&str>>, ptr: Option<&str>, live: bool) -> AddressFacts {
        AddressFacts {
            addr: addr.parse().unwrap(),
            netbox: nb.map(|name| NetBoxRecord { dns_name: name.map(str::to_string) }),
            ptr: ptr.map(str::to_string),
            live,
        }
    }

    #[test]
    fn snapshot_round_trips_through_text() {
        let snap = Snapshot {
            zone: "nfra.nl".into(),
            view: "dns1".into(),
            serial: 2026070300,
            synced: 1_700_000_000,
            facts: vec![
                fact("10.87.3.68", None, Some("dop21-ipmi.nfra.nl"), true), // DNS-only
                fact("10.87.3.131", Some(None), None, false),               // NetBox reservation, no name
                fact("2001:db8::1", Some(Some("h1.nfra.nl")), Some("h1.nfra.nl"), true), // dual, v6
            ],
        };
        let back = Snapshot::from_text(&snap.to_text()).unwrap();
        // The parse recovers the header and every fact; order is normalised (sorted by address).
        assert_eq!(back.zone, "nfra.nl");
        assert_eq!(back.view, "dns1");
        assert_eq!(back.serial, 2026070300);
        assert_eq!(back.synced, 1_700_000_000);
        let mut expected = snap.facts.clone();
        expected.sort_by_key(|f| f.addr);
        assert_eq!(back.facts, expected);
    }

    #[test]
    fn text_is_deterministic_and_git_friendly() {
        // Same facts in a different input order produce identical bytes (sorted), so git diffs clean.
        let a = Snapshot { zone: "z".into(), view: "v".into(), serial: 1, synced: 2, facts: vec![fact("10.0.0.2", None, None, true), fact("10.0.0.1", None, None, false)] };
        let b = Snapshot { facts: vec![fact("10.0.0.1", None, None, false), fact("10.0.0.2", None, None, true)], ..a.clone() };
        assert_eq!(a.to_text(), b.to_text());
    }

    #[test]
    fn freshness_is_serial_equality() {
        assert!(is_fresh(Some(2026070300), 2026070300)); // unchanged → skip the AXFR
        assert!(!is_fresh(Some(2026070300), 2026070301)); // bumped → transfer
        assert!(!is_fresh(None, 1)); // never cached → transfer
    }
}
