// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP map: lay an address range onto a **generalized-Hilbert (Gilbert) curve** so the
//! grid fills the whole terminal rectangle — any `width × height`, not just a square power
//! of two — while consecutive addresses stay spatially adjacent (the curve never jumps,
//! unlike Z-order), so a contiguous allocation reads as one blob.
//!
//! A range is mapped over its **full** `2^host_bits` address block (not the usable hosts,
//! whose count isn't a power of two) so the address line tiles the grid evenly. Works for
//! IPv4 and IPv6 alike — all the offset arithmetic is `u128`. Each cell covers a contiguous
//! [`AddrRange`] slice of `block_len / (width·height)` addresses; when the grid dimensions
//! and block size are both powers of two the slice happens to be a clean CIDR, otherwise it
//! is a ragged run with no single prefix (the price of filling an arbitrary rectangle). An
//! enumerable range is shaded by absolute occupancy; a sparse IPv6 range by *relative*
//! density (the busiest cell is hottest) since absolute occupancy of a `2^128` block is
//! meaningless. Building is `O(width·height + facts)`. Rendering lives in `tui::map`.

use std::collections::HashMap;
use std::net::IpAddr;

use mullion::spacefill::Gilbert;

use crate::reconcile::{AddrRange, AddressFacts};

/// A Gilbert-laid map of an address range: a `width × height` grid whose cells, in curve
/// order, tile the range's full address block; `used[d]` counts used addresses in the cell
/// at curve distance `d`.
#[derive(Debug, Clone)]
pub struct MapGrid {
    /// The address range this map covers.
    pub range: AddrRange,
    /// Grid width in cells.
    pub width: u32,
    /// Grid height in cells.
    pub height: u32,
    /// The Gilbert curve mapping over `width × height` — `d ↔ (x, y)` both ways in O(1).
    gilbert: Gilbert,
    /// Used-address count per cell, indexed by **curve distance** `d`.
    pub used: Vec<u32>,
    /// `true` when the range is too large to reason about absolute occupancy (a sparse
    /// IPv6 block): [`fraction`](MapGrid::fraction) then reports *relative* density.
    sparse: bool,
    /// The busiest cell's used count — the denominator for relative density.
    max_used: u32,
}

impl MapGrid {
    /// Build the `width × height` map for `range` from the (bounded) `facts`.
    ///
    /// The caller sizes the grid (see `tui::map::fit_dims`) so `width·height ≤ block_len`
    /// — every cell holds at least one address. Each known address is bucketed into its
    /// curve cell by [`AddrRange::slice_index`], all in `u128` so an IPv6 range works the
    /// same. `O(width·height + facts)`.
    #[must_use]
    pub fn build(range: AddrRange, facts: &HashMap<IpAddr, AddressFacts>, width: u32, height: u32) -> MapGrid {
        let gilbert = Gilbert::new(width, height);
        let cells = gilbert.len() as u128;

        let mut used = vec![0u32; gilbert.len()];
        if cells > 0 {
            for addr in facts.keys() {
                if let Some(off) = range.offset_of(*addr) {
                    let d = range.slice_index(cells, off) as usize;
                    used[d.min(gilbert.len().saturating_sub(1))] += 1;
                }
            }
        }
        let max_used = used.iter().copied().max().unwrap_or(0);
        MapGrid { range, width, height, gilbert, used, sparse: !range.is_enumerable(), max_used }
    }

    /// Number of cells (`width · height`).
    #[must_use]
    pub fn cells(&self) -> usize {
        self.gilbert.len()
    }

    /// The address slice cell `d` covers — one of `cells()` near-equal contiguous runs of
    /// the range. A clean CIDR when the geometry is a power of two, else a ragged run.
    #[must_use]
    pub fn cell_range(&self, d: usize) -> AddrRange {
        self.range.nth_slice(self.cells() as u128, d as u128)
    }

    /// The heat value of cell `d` in `[0, 1]`.
    ///
    /// For an enumerable range this is **absolute** occupancy (used addresses over the
    /// cell's address count) — "how full is this block". For a sparse IPv6 range, where a
    /// cell spans up to `2^128` addresses and absolute occupancy is always ≈0, it is
    /// instead **relative** density on a log scale (busiest cell = 1), so the map shows
    /// *where* allocations cluster rather than a uniform near-empty field.
    #[must_use]
    pub fn fraction(&self, d: usize) -> f32 {
        let used = self.used[d];
        if used == 0 {
            return 0.0;
        }
        if self.sparse {
            if self.max_used <= 1 {
                return 1.0;
            }
            (f64::from(used).ln_1p() / f64::from(self.max_used).ln_1p()) as f32
        } else {
            (f64::from(used) / self.cell_range(d).block_len() as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// The grid position `(x, y)` of cell `d` (its Gilbert placement).
    #[must_use]
    pub fn cell_xy(&self, d: usize) -> (u32, u32) {
        self.gilbert.d_to_xy(d)
    }

    /// The curve distance `d` at grid cell `(x, y)`, or `None` if off-grid — turns a
    /// cursor position into the address slice under it.
    #[must_use]
    pub fn xy_to_d(&self, x: u32, y: u32) -> Option<u32> {
        self.gilbert.xy_to_d(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::Cidr;

    fn fact(addr: &str) -> AddressFacts {
        AddressFacts { addr: addr.parse().unwrap(), netbox: None, ptr: Some("x.".into()), live: false }
    }

    fn range(s: &str) -> AddrRange {
        AddrRange::from(Cidr::parse(s).unwrap())
    }

    #[test]
    fn ipv6_map_buckets_and_uses_relative_density() {
        // A /32 is far too big for absolute occupancy; the heat must be *relative* so a
        // busy cell stands out. Two clusters land in two different curve cells.
        let facts: HashMap<_, _> =
            [fact("2001:db8::1"), fact("2001:db8::2"), fact("2001:db8::3"), fact("2001:db8:ffff::1")]
                .into_iter()
                .map(|f| (f.addr, f))
                .collect();
        let g = MapGrid::build(range("2001:db8::/32"), &facts, 16, 16);
        assert_eq!(g.used.iter().sum::<u32>(), 4); // all four bucketed
        assert_eq!(g.used.iter().filter(|&&u| u > 0).count(), 2); // in two cells
        let heats: Vec<f32> = (0..g.cells()).map(|d| g.fraction(d)).collect();
        let max = heats.iter().copied().fold(0.0_f32, f32::max);
        assert!((max - 1.0).abs() < 1e-6, "busiest cell should be full heat");
        // The lone-host cell is warm but not maxed (relative log scale).
        assert!(heats.iter().any(|&h| h > 0.0 && h < 0.99));
    }

    #[test]
    fn grid_tiles_every_cell_once_and_stays_contiguous() {
        // A Gilbert grid over an arbitrary (non-square) rectangle: every cell placed once,
        // and consecutive curve cells are unit-adjacent (locality — the point of the curve).
        for (w, h) in [(16u32, 16u32), (12, 8), (40, 24)] {
            let g = MapGrid::build(range("10.0.0.0/8"), &HashMap::new(), w, h);
            let mut seen = vec![false; (w * h) as usize];
            let mut prev: Option<(u32, u32)> = None;
            for d in 0..g.cells() {
                let (x, y) = g.cell_xy(d);
                assert!(x < w && y < h);
                assert_eq!(g.xy_to_d(x, y), Some(d as u32)); // round-trips
                let i = (y * w + x) as usize;
                assert!(!seen[i], "cell ({x},{y}) visited twice");
                seen[i] = true;
                if let Some((px, py)) = prev {
                    assert_eq!(px.abs_diff(x) + py.abs_diff(y), 1, "d={d} jumped (not contiguous)");
                }
                prev = Some((x, y));
            }
            assert!(seen.into_iter().all(|b| b), "{w}x{h} left a gap");
        }
    }

    #[test]
    fn power_of_two_geometry_gives_clean_cidr_cells() {
        // A /8 on a 16×16 grid → 256 cells, each a clean /16; cell 0 is 10.0.0.0/16 and the
        // cells tile the block in curve order.
        let g = MapGrid::build(range("10.0.0.0/8"), &HashMap::new(), 16, 16);
        assert_eq!(g.cells(), 256);
        let c0 = g.cell_range(0).as_cidr().expect("aligned geometry → clean CIDR");
        assert_eq!(c0.prefix_len, 16);
        assert_eq!(c0.base, "10.0.0.0".parse::<IpAddr>().unwrap());
        // The 256 cells partition the /8 into the 256 distinct /16s.
        let firsts: std::collections::HashSet<_> =
            (0..g.cells()).map(|d| g.cell_range(d).base()).collect();
        assert_eq!(firsts.len(), 256);
    }

    #[test]
    fn ragged_geometry_still_covers_the_whole_block() {
        // A non-power-of-two grid can't give clean CIDRs, but the slices must still tile the
        // range with no gap: the last cell's last address is the range's last address.
        let r = range("10.87.3.0/24");
        let g = MapGrid::build(r, &HashMap::new(), 7, 5); // 35 cells over 256 addresses
        assert_eq!(g.cell_range(0).base(), r.base());
        assert_eq!(g.cell_range(g.cells() - 1).last(), r.last());
        // 256 doesn't divide by 35, so some cells are ragged runs with no clean prefix.
        assert!((0..g.cells()).any(|d| g.cell_range(d).as_cidr().is_none()));
    }

    #[test]
    fn buckets_facts_into_curve_cells() {
        let facts: HashMap<_, _> = [fact("10.0.0.9"), fact("10.1.2.3")]
            .into_iter()
            .map(|f| (f.addr, f))
            .collect();
        let g = MapGrid::build(range("10.0.0.0/8"), &facts, 16, 16); // /16 cells
        // 10.0.x → cell 0 (offset < 65536); 10.1.x → cell 1.
        assert_eq!(g.used[0], 1);
        assert_eq!(g.used[1], 1);
        assert_eq!(g.used.iter().sum::<u32>(), 2);
    }
}
