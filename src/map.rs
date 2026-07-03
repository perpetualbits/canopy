// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP map: aggregate an address range into a grid of "little squares", each
//! standing for a **block** of consecutive addresses and shaded by how much of that
//! block is used. It's the overview for a space too large to show one row per
//! address — a `/8` (16M) as a `32×32` grid is 1024 cells of ~16k addresses each
//! (see `docs/vision.md`). The model is pure; rendering lives in `tui::map`.
//!
//! "Used" means any source reported the address (it is in `facts`); everything else
//! is free. Building is `O(facts)` — each known address is bucketed into its cell,
//! never the whole range.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use crate::reconcile::{AddressFacts, Cidr};

/// A `cols × rows` grid over a range: each cell aggregates `block` consecutive host
/// addresses and holds the count that are used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapGrid {
    /// Number of columns.
    pub cols: usize,
    /// Number of rows.
    pub rows: usize,
    /// Total usable host addresses in the range.
    pub total: u64,
    /// Addresses per cell (the last populated cell may hold fewer).
    pub block: u64,
    /// Used-address count per cell, row-major (`len == cols * rows`).
    pub used: Vec<u32>,
}

impl MapGrid {
    /// Build the grid for `range` from the (bounded) `facts`, aggregated into
    /// `cols × rows` cells.
    ///
    /// How: the block size is `⌈total / cells⌉`, so the whole range fits; each known
    /// address is placed in cell `host_index / block`. `O(facts)`, never `O(range)`.
    #[must_use]
    pub fn build(range: Cidr, facts: &HashMap<Ipv4Addr, AddressFacts>, cols: usize, rows: usize) -> MapGrid {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let total = range.host_count();
        let cells = (cols * rows) as u64;
        let block = total.div_ceil(cells).max(1);

        let mut used = vec![0u32; cols * rows];
        for addr in facts.keys() {
            if let Some(idx) = range.host_index(*addr) {
                let cell = (idx / block).min(cells - 1) as usize;
                used[cell] += 1;
            }
        }
        MapGrid { cols, rows, total, block, used }
    }

    /// The number of addresses cell `i` actually covers — `block`, except a trailing
    /// cell that runs past the end of the range (or 0 for a cell entirely past it).
    #[must_use]
    pub fn cell_size(&self, i: usize) -> u64 {
        let start = i as u64 * self.block;
        self.block.min(self.total.saturating_sub(start))
    }

    /// The used fraction of cell `i` in `[0, 1]`: used addresses over the cell's
    /// covered size. A cell that covers no addresses reads as `0.0` (free).
    #[must_use]
    pub fn fraction(&self, i: usize) -> f32 {
        let size = self.cell_size(i);
        if size == 0 {
            0.0
        } else {
            (f64::from(self.used[i]) / size as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// The first host address cell `i` covers — its position in address space, for
    /// axis labels and (later) zoom-in.
    #[must_use]
    pub fn cell_start(&self, range: Cidr, i: usize) -> Ipv4Addr {
        let idx = (i as u64 * self.block).min(self.total.saturating_sub(1));
        range.host_at(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(addr: &str) -> AddressFacts {
        AddressFacts { addr: addr.parse().unwrap(), netbox: None, ptr: Some("x.".into()), live: false }
    }

    #[test]
    fn aggregates_facts_into_cells() {
        // /24 → 254 hosts; a 2×2 grid = 4 cells; block = ceil(254/4) = 64.
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let facts: HashMap<_, _> = [fact("10.87.3.1"), fact("10.87.3.2"), fact("10.87.3.130")]
            .into_iter()
            .map(|f| (f.addr, f))
            .collect();
        let g = MapGrid::build(range, &facts, 2, 2);
        assert_eq!(g.block, 64);
        // .1 and .2 are host indices 0,1 → cell 0; .130 is index 129 → cell 2.
        assert_eq!(g.used[0], 2);
        assert_eq!(g.used[2], 1);
        assert_eq!(g.used[1], 0);
        assert!(g.fraction(0) > g.fraction(2)); // cell 0 is denser
        assert_eq!(g.cell_start(range, 0), "10.87.3.1".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn a_slash_8_grid_is_cheap() {
        // 16,777,214 hosts into 32×32 = 1024 cells, ~16k each — O(facts), not O(range).
        let range = Cidr::parse("10.0.0.0/8").unwrap();
        let facts: HashMap<_, _> = [fact("10.0.0.1"), fact("10.128.0.9")]
            .into_iter()
            .map(|f| (f.addr, f))
            .collect();
        let g = MapGrid::build(range, &facts, 32, 32);
        assert_eq!(g.used.len(), 1024);
        assert_eq!(g.block, 16_384);
        assert_eq!(g.used[0], 1); // 10.0.0.1 near the start
        assert_eq!(g.used.iter().map(|&u| u as u32).sum::<u32>(), 2);
    }
}
