// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Turning reconciled addresses into a **node graph**: hosts grouped into named
//! **clusters** (`ntserver`, `netapp`, `dop`, `iprotect`, …), with an edge from each
//! host to its cluster. This is the first cut of the vision's "clusters of computers
//! are a named grouping — a quick overview": mullion's `sugiyama::auto_layout` places
//! the nodes into tidy, few-crossing layers, and the TUI draws them.
//!
//! The model is pure; layout (which needs mullion) is a thin method on top so the
//! grouping logic stays testable without a terminal.

use std::collections::BTreeMap;

use mullion::sugiyama::{auto_layout, LayerDir, SugiyamaParams};
use mullion::{FloatRect, GraphCanvas, TileId};

use crate::reconcile::AddressRow;

/// What a graph node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A named cluster (group) of hosts.
    Cluster,
    /// A single host.
    Host,
}

/// One node: a stable id, a display label, and what it is.
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// Stable id, also used as the mullion `TileId`.
    pub id: TileId,
    /// Text drawn inside the node box.
    pub label: String,
    /// Cluster or host.
    pub kind: NodeKind,
}

/// A graph of hosts grouped under clusters, plus the host→cluster edges.
#[derive(Debug, Clone, Default)]
pub struct DnsGraph {
    /// All nodes (clusters first, then hosts).
    pub nodes: Vec<GraphNode>,
    /// Directed edges `(cluster_id, host_id)` — a cluster contains its hosts. The
    /// direction puts clusters on the top layer under Sugiyama, hosts below.
    pub edges: Vec<(TileId, TileId)>,
}

impl DnsGraph {
    /// Build the cluster graph from reconciled rows.
    ///
    /// How: every row that has a name is grouped by [`cluster_of`]; each distinct
    /// cluster becomes one node (labelled with its member count), each named host
    /// becomes a node, and an edge ties the host to its cluster. Free/nameless rows
    /// are ignored — they carry no structure to show.
    #[must_use]
    pub fn from_rows(rows: &[AddressRow]) -> DnsGraph {
        // Collect (host label, cluster name), in address order.
        let hosts: Vec<(String, String)> = rows
            .iter()
            .filter_map(|r| r.name.as_ref().map(|n| (short_label(n), cluster_of(n))))
            .collect();

        // Count members per cluster (BTreeMap → deterministic, alphabetical).
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (_, c) in &hosts {
            *counts.entry(c.clone()).or_insert(0) += 1;
        }

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut next_id: TileId = 1;

        // Cluster nodes first, so they take the low ids (and the first layer).
        let mut cluster_id: BTreeMap<String, TileId> = BTreeMap::new();
        for (name, n) in &counts {
            let id = next_id;
            next_id += 1;
            cluster_id.insert(name.clone(), id);
            nodes.push(GraphNode { id, label: format!("{name} ({n})"), kind: NodeKind::Cluster });
        }

        // Host nodes + edges to their cluster.
        for (label, cluster) in &hosts {
            let id = next_id;
            next_id += 1;
            nodes.push(GraphNode { id, label: label.clone(), kind: NodeKind::Host });
            edges.push((cluster_id[cluster], id));
        }

        DnsGraph { nodes, edges }
    }

    /// Number of distinct clusters.
    #[must_use]
    pub fn cluster_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.kind == NodeKind::Cluster).count()
    }

    /// Lay the graph out with mullion's Sugiyama layered algorithm and return the
    /// populated canvas. The canvas is sized to fit the content (clusters on the top
    /// layer, hosts below), so a caller can pan a smaller window across it.
    #[must_use]
    pub fn layout(&self) -> GraphCanvas {
        // Widest layer decides the canvas width; two layers decide the height.
        let span = |kind: NodeKind| -> u16 {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .map(|n| node_width(&n.label) + 2)
                .sum::<u16>()
                .max(20)
        };
        let w = span(NodeKind::Cluster).max(span(NodeKind::Host)) + 4;
        let h: u16 = 20; // two layers of height 3 plus generous gaps/slack

        let mut canvas = GraphCanvas::new(w, h);
        for node in &self.nodes {
            canvas.add(node.id, FloatRect::new(0, 0, node_width(&node.label), 3));
        }
        let params = SugiyamaParams { dir: LayerDir::TopDown, layer_gap: 3, node_gap: 2, grid: 1 };
        auto_layout(&mut canvas, &self.edges, &params);
        canvas
    }

    /// Look up a node by id (for labels/colours at render time).
    #[must_use]
    pub fn node(&self, id: TileId) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// The cluster a hostname belongs to: its leading run of ASCII letters, e.g.
/// `ntserver56-ipmi` → `ntserver`, `netapp-dw1-bmc` → `netapp`. Names that do not
/// start with a letter fall in `misc`.
#[must_use]
pub fn cluster_of(fqdn: &str) -> String {
    let first = fqdn.split('.').next().unwrap_or(fqdn);
    let prefix: String = first.chars().take_while(char::is_ascii_alphabetic).collect();
    if prefix.is_empty() {
        "misc".to_string()
    } else {
        prefix
    }
}

/// The host's short label: the first DNS label (drop the domain).
fn short_label(fqdn: &str) -> String {
    fqdn.split('.').next().unwrap_or(fqdn).to_string()
}

/// Box width for a label: text + two border columns, clamped to something sane.
fn node_width(label: &str) -> u16 {
    (label.chars().count() as u16 + 2).clamp(6, 28)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{reconcile, AddressFacts, Cidr};

    fn ptr_row(oct: u8, name: &str) -> AddressFacts {
        AddressFacts {
            addr: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 87, 3, oct)),
            netbox: None,
            ptr: Some(format!("{name}.")),
            live: false,
        }
    }

    #[test]
    fn cluster_derivation() {
        assert_eq!(cluster_of("ntserver56-ipmi.nfra.nl"), "ntserver");
        assert_eq!(cluster_of("netapp-dw1-bmc.nfra.nl"), "netapp");
        assert_eq!(cluster_of("dop21-ipmi.nfra.nl"), "dop");
        assert_eq!(cluster_of("5-assig.nfra.nl"), "misc"); // starts with a digit
    }

    #[test]
    fn graph_groups_hosts_under_clusters() {
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let facts = vec![
            ptr_row(68, "dop21-ipmi.nfra.nl"),
            ptr_row(76, "dop75-ipmi.nfra.nl"),
            ptr_row(71, "ntserver56-ipmi.nfra.nl"),
        ];
        let rows = reconcile(range, &facts);
        let g = DnsGraph::from_rows(&rows);

        // 2 clusters (dop, ntserver) + 3 hosts = 5 nodes; 3 host→cluster edges.
        assert_eq!(g.cluster_count(), 2);
        assert_eq!(g.nodes.len(), 5);
        assert_eq!(g.edges.len(), 3);
        // A dop host edges to the dop cluster.
        let dop = g.nodes.iter().find(|n| n.label.starts_with("dop ")).unwrap();
        assert!(g.edges.iter().filter(|(c, _)| *c == dop.id).count() == 2);
    }

    #[test]
    fn layout_places_every_node_in_bounds() {
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let facts = vec![ptr_row(68, "dop21-ipmi.nfra.nl"), ptr_row(71, "ntserver56-ipmi.nfra.nl")];
        let rows = reconcile(range, &facts);
        let g = DnsGraph::from_rows(&rows);
        let canvas = g.layout();
        // Every node got a placement.
        for n in &g.nodes {
            assert!(canvas.place(n.id).is_some(), "node {} unplaced", n.id);
        }
    }
}
