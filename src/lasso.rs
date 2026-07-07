// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The **lasso selection**, reduced to meaning — pure, no I/O and no drawing.
//!
//! On the Gilbert map you lasso an area (usually a *snake* — a contiguous run of curve cells, which
//! is exactly a clean address range) and a floating callout hangs off it: name, IPv4 range, IPv6
//! range, cluster/group, VLAN. This module owns the **meaning** half of that feature (the split
//! agreed with mullion, which owns the drawing: the temporal-composited outline + routed leader +
//! floating box). Given the address span(s) the lassoed cells cover, it reads the merged facts, the
//! grouping, and the subnet list and reduces them to the callout's short lines.
//!
//! The map shows **one family at a time**, but a callout wants *both* v4 and v6. The other family is
//! recovered by a **name-join**: the selected hosts have names (from NetBox or PTR); any address in
//! the other family that shares a name is the same logical host, so its address joins the footprint.
//! This is exactly canopy's dual-stack reconciliation, applied to a selection.
//!
//! Forward-declared: the meaning half is built and tested here; the gesture (lasso a snake, cycle
//! the snap granularity) and the draw (mullion's `temporal_overlay` + `callout`) wire it in next. The
//! `allow` keeps the build warning-free until then.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use crate::group::Grouping;
use crate::reconcile::{AddrRange, AddressFacts, Subnet};

/// A resolved lasso: the facts a floating callout needs, already reduced from the selected cells.
/// Every line but `title` is optional — absent when the selection has nothing to say for it (no
/// grouped member, no covering subnet, no counterpart in the other family).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary {
    /// The headline: the subnet name if the selection *is* exactly one subnet, else the single
    /// cluster it falls in, else the covered range's label.
    pub title: String,
    /// The IPv4 range line — the selected block(s) when the map is v4, else the v4 host footprint
    /// recovered by name-join.
    pub ipv4: Option<String>,
    /// The IPv6 range line — the selected block(s) when the map is v6, else the v6 host footprint
    /// recovered by name-join.
    pub ipv6: Option<String>,
    /// The cluster/group the selection belongs to (or `"first (N groups)"` when it spans several).
    pub cluster: Option<String>,
    /// The VLAN/subnet the selection sits in (from the subnet's human name).
    pub vlan: Option<String>,
    /// How many known hosts the selection covers.
    pub hosts: usize,
}

impl Summary {
    /// The callout lines, in priority order — title first, then whichever of v4/v6/cluster/VLAN are
    /// present. The floating box shows as many as fit (the feature calls for 1–3); the caller
    /// truncates. Kept short and label-prefixed so a narrow box still reads.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![self.title.clone()];
        // Skip any line that just repeats the title (which is often the CIDR, subnet, or cluster
        // name already) — the callout should read clean, not echo itself.
        let mut push = |label: &str, val: &Option<String>| {
            if let Some(x) = val {
                if *x != self.title {
                    v.push(format!("{label}{x}"));
                }
            }
        };
        push("v4  ", &self.ipv4);
        push("v6  ", &self.ipv6);
        push("grp ", &self.cluster);
        push("vlan ", &self.vlan);
        v
    }
}

/// The host name a source reports for an address: NetBox's DNS name if present, else the PTR, with
/// any trailing dot stripped. `None` when neither source named it — such a host can't be joined
/// across families (no name to match on).
fn host_name(f: &AddressFacts) -> Option<String> {
    f.netbox
        .as_ref()
        .and_then(|n| n.dns_name.clone())
        .or_else(|| f.ptr.clone())
        .map(|s| s.trim_end_matches('.').to_string())
}

/// Format a set of addresses as a compact footprint line: the lone address, or `lo – hi (N hosts)`.
fn footprint(mut addrs: Vec<IpAddr>) -> Option<String> {
    addrs.sort_unstable();
    addrs.dedup();
    match addrs.len() {
        0 => None,
        1 => Some(addrs[0].to_string()),
        n => Some(format!("{} – {}  ({n} hosts)", addrs[0], addrs[n - 1])),
    }
}

/// Reduce a lasso selection to a callout [`Summary`]. Pure.
///
/// `ranges` are the address spans the lassoed cells cover (usually one contiguous snake, but a 2-D
/// blob can be several). `facts` is the *merged* fact set across both families — the v6-by-name
/// join needs the other family present. The selected family (the one the `ranges` are in) is shown
/// as its block label; the other family is the name-joined host footprint.
#[must_use]
pub fn summarize(ranges: &[AddrRange], facts: &HashMap<IpAddr, AddressFacts>, grouping: &Grouping, subnets: &[Subnet]) -> Summary {
    // Members: every known host whose address falls inside the selection.
    let mut members: Vec<&AddressFacts> = facts.values().filter(|f| ranges.iter().any(|r| r.contains(f.addr))).collect();
    members.sort_by_key(|f| f.addr);

    // The names in the selection seed the cross-family join.
    let sel_names: HashSet<String> = members.iter().filter_map(|f| host_name(f)).collect();
    let mut v4: Vec<IpAddr> = Vec::new();
    let mut v6: Vec<IpAddr> = Vec::new();
    // Every fact (selected or not) that shares a selected host's name contributes its address to
    // that family's footprint — so a v4 lasso surfaces the hosts' AAAA addresses, and vice-versa.
    for f in facts.values() {
        let in_sel = ranges.iter().any(|r| r.contains(f.addr));
        let joined = host_name(f).is_some_and(|n| sel_names.contains(&n));
        if !in_sel && !joined {
            continue;
        }
        match f.addr {
            IpAddr::V4(_) => v4.push(f.addr),
            IpAddr::V6(_) => v6.push(f.addr),
        }
    }

    // The selected family shows its block label(s); the other family shows the joined footprint.
    let sel_v6 = ranges.first().is_some_and(|r| r.base().is_ipv6());
    let block_label = || {
        let l = ranges.iter().map(|r| r.label()).collect::<Vec<_>>().join(", ");
        (!l.is_empty()).then_some(l)
    };
    let (ipv4, ipv6) = if sel_v6 { (footprint(v4), block_label()) } else { (block_label(), footprint(v6)) };

    // Clusters the members belong to (distinct, by human label).
    let mut clusters: Vec<String> = members.iter().filter_map(|f| grouping.group_of(f.addr).map(|g| g.label.clone())).collect();
    clusters.sort();
    clusters.dedup();
    let cluster = match clusters.len() {
        0 => None,
        1 => Some(clusters[0].clone()),
        n => Some(format!("{} ({n} groups)", clusters[0])),
    };

    // VLAN/subnet: the most-specific covering subnet's name for each member, distinct and non-empty.
    let mut vlans: Vec<String> = members
        .iter()
        .filter_map(|f| Subnet::most_specific(subnets, f.addr))
        .filter(|s| !s.name.is_empty())
        .map(|s| s.name.clone())
        .collect();
    vlans.sort();
    vlans.dedup();
    let vlan = match vlans.len() {
        0 => None,
        1 => Some(vlans[0].clone()),
        n => Some(format!("{} (+{})", vlans[0], n - 1)),
    };

    // Title: the subnet name if the selection is exactly one subnet, else a lone cluster, else the
    // covered range label.
    let exact_subnet = (ranges.len() == 1)
        .then(|| ranges[0].as_cidr())
        .flatten()
        .and_then(|c| subnets.iter().find(|s| s.cidr == c));
    let title = if let Some(s) = exact_subnet {
        if s.name.is_empty() {
            format!("{}/{}", s.cidr.base, s.cidr.prefix_len)
        } else {
            s.name.clone()
        }
    } else if clusters.len() == 1 {
        clusters[0].clone()
    } else {
        ranges.iter().map(|r| r.label()).collect::<Vec<_>>().join(", ")
    };

    Summary { title, ipv4, ipv6, cluster, vlan, hosts: members.len() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::Cidr;

    fn facts_of(list: &[AddressFacts]) -> HashMap<IpAddr, AddressFacts> {
        list.iter().map(|f| (f.addr, f.clone())).collect()
    }

    fn named(addr: &str, name: &str) -> AddressFacts {
        AddressFacts { addr: addr.parse().unwrap(), netbox: None, ptr: Some(format!("{name}.")), live: true }
    }

    fn range(cidr: &str) -> AddrRange {
        AddrRange::from(Cidr::parse(cidr).unwrap())
    }

    #[test]
    fn summarize_reports_block_hosts_and_cluster() {
        // Two hosts of a name family (netapp-dw*) plus a stray, all inside one /26.
        let facts = facts_of(&[named("10.87.3.10", "netapp-dw01"), named("10.87.3.11", "netapp-dw02"), named("10.87.3.40", "printer5")]);
        let grouping = crate::group::merge(Vec::new(), Vec::new(), crate::group::infer(&facts));
        let s = summarize(&[range("10.87.3.0/26")], &facts, &grouping, &[]);

        assert_eq!(s.hosts, 3, "all three hosts fall in the /26");
        assert_eq!(s.ipv4.as_deref(), Some("10.87.3.0/26"), "v4 line is the selected block");
        assert!(s.ipv6.is_none(), "no v6 facts → no v6 line");
        assert!(s.cluster.as_deref().is_some_and(|c| c.contains("netapp")), "the netapp-dw family is surfaced");
    }

    #[test]
    fn dual_stack_recovers_v6_by_name_join() {
        // A v4 host and its AAAA share a name; lassoing the v4 block surfaces the v6 address.
        let facts = facts_of(&[named("10.0.0.1", "h1"), named("2001:db8::1", "h1"), named("10.0.0.2", "h2")]);
        let grouping = crate::group::merge(Vec::new(), Vec::new(), crate::group::infer(&facts));
        let s = summarize(&[range("10.0.0.0/30")], &facts, &grouping, &[]);

        assert_eq!(s.ipv4.as_deref(), Some("10.0.0.0/30"), "v4 block is the selection");
        assert_eq!(s.ipv6.as_deref(), Some("2001:db8::1"), "v6 recovered by joining on the shared name h1");
    }

    #[test]
    fn title_and_vlan_come_from_an_exact_subnet() {
        let facts = facts_of(&[named("10.87.3.5", "srv-a")]);
        let grouping = crate::group::merge(Vec::new(), Vec::new(), crate::group::infer(&facts));
        let subnets = vec![Subnet { cidr: Cidr::parse("10.87.3.0/26").unwrap(), name: "srv-vlan-42".into() }];
        let s = summarize(&[range("10.87.3.0/26")], &facts, &grouping, &subnets);

        assert_eq!(s.title, "srv-vlan-42", "selection is exactly the subnet → its name is the title");
        assert_eq!(s.vlan.as_deref(), Some("srv-vlan-42"), "the covering subnet is the VLAN line");
    }

    #[test]
    fn v6_map_selection_labels_v6_and_joins_v4() {
        // The reciprocal: a v6 lasso shows the v6 block and recovers the v4 counterpart by name.
        let facts = facts_of(&[named("2001:db8::1", "h1"), named("10.9.9.9", "h1")]);
        let grouping = crate::group::merge(Vec::new(), Vec::new(), crate::group::infer(&facts));
        let s = summarize(&[range("2001:db8::/126")], &facts, &grouping, &[]);

        assert_eq!(s.ipv6.as_deref(), Some("2001:db8::/126"), "v6 block is the selection");
        assert_eq!(s.ipv4.as_deref(), Some("10.9.9.9"), "v4 recovered by name-join");
    }
}
