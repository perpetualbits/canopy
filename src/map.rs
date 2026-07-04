// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP map: lay an address range onto a **Hilbert curve** so that every
//! axis-aligned power-of-two square is a contiguous CIDR — a zoomed square is always
//! a clean subnet — while consecutive addresses stay spatially adjacent (the curve
//! never jumps, unlike Z-order), so a contiguous allocation reads as one blob.
//!
//! A range is mapped over its **full** `2^host_bits` address block (not the usable
//! hosts, whose count isn't a power of two) so the quadrant tiling is exact. Works for
//! IPv4 and IPv6 alike — all the offset arithmetic is `u128`. The grid is
//! `2^order × 2^order`; each cell covers a `/(prefix + 2·order)` block. An enumerable
//! range is shaded by absolute occupancy; a sparse IPv6 range by *relative* density (the
//! busiest cell is hottest) since absolute occupancy of a `2^128` block is meaningless.
//! Building is `O(facts)`. Rendering lives in `tui::map`.

use std::collections::HashMap;
use std::net::IpAddr;

use crate::reconcile::{AddressFacts, Cidr};

/// Map a Hilbert distance `d` (`0..4^order`) to its grid cell `(x, y)` on the
/// order-`order` Hilbert curve — a `2^order × 2^order` grid.
///
/// Standard iterative construction: read `d` two bits at a time from the top, and at
/// each level rotate/reflect the current sub-square so the curve joins up. Bijective
/// with [`hilbert_xy2d`]; a `+1` step in `d` is always an adjacent cell.
#[must_use]
pub fn hilbert_d2xy(order: u32, d: u64) -> (u32, u32) {
    let n = 1u64 << order;
    let (mut x, mut y, mut t) = (0u64, 0u64, d);
    let mut s = 1u64;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        // Rotate/reflect this sub-square of side `s` before placing the quadrant.
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        x += s * rx;
        y += s * ry;
        t /= 4;
        s *= 2;
    }
    (x as u32, y as u32)
}

/// Map a grid cell `(x, y)` back to its Hilbert distance `d` — the inverse of
/// [`hilbert_d2xy`], used to turn a cursor position into the address block under it.
#[must_use]
pub fn hilbert_xy2d(order: u32, x: u32, y: u32) -> u64 {
    let n = 1u64 << order;
    let (mut x, mut y) = (u64::from(x), u64::from(y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from((x & s) > 0);
        let ry = u64::from((y & s) > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Rotate/reflect the full grid of side `n` (mirror of the d2xy step).
        if ry == 0 {
            if rx == 1 {
                x = n - 1 - x;
                y = n - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// The clean CIDR block that cell `d` of an order-`order` grid over `range` covers —
/// a `/(prefix + 2·order)`. This is the "a zoomed square is always a subnet" guarantee
/// made concrete, and the single source of truth for both the grid and the map cursor.
///
/// A cell holds `block = 2^(host_bits − 2·order)` addresses, so cell `d` starts at offset
/// `d · block` from the range's network address; its prefix grows by `2·order` bits. The
/// offset is a shift (not a multiply) so it never overflows even for a `/0` IPv6 block.
#[must_use]
pub fn block_cidr(range: Cidr, order: u32, d: u64) -> Cidr {
    let shift = range.host_bits() - 2 * order; // host bits below the cell
    let offset = if shift >= 128 { 0 } else { u128::from(d) << shift };
    Cidr { base: range.address_at_offset(offset), prefix_len: range.prefix_len + 2 * order as u8 }
}

/// A Hilbert-laid map of a CIDR range: a `2^order × 2^order` grid whose cells, in
/// Hilbert order, tile the range's address block; `used[d]` counts used addresses in
/// cell `d`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapGrid {
    /// The CIDR this map covers.
    pub range: Cidr,
    /// Grid order — side is `2^order`, cell count `4^order`.
    pub order: u32,
    /// Addresses per cell — `2^(host_bits − 2·order)`, a power of two (`u128` because an
    /// IPv6 cell can span up to `2^128` addresses).
    pub block: u128,
    /// Used-address count per cell, indexed by **Hilbert distance** `d`.
    pub used: Vec<u32>,
    /// `true` when the range is too large to reason about absolute occupancy (a sparse
    /// IPv6 block): [`fraction`](MapGrid::fraction) then reports *relative* density.
    sparse: bool,
    /// The busiest cell's used count — the denominator for relative density.
    max_used: u32,
}

impl MapGrid {
    /// Build the map for `range` from the (bounded) `facts`, at the largest order up
    /// to `max_order` that still gives at least one address per cell.
    ///
    /// The order is capped at `host_bits / 2` so `2·order ≤ host_bits` (a cell is a
    /// real, ≥1-address CIDR). Each known address is bucketed into cell `offset ≫ shift`
    /// (its Hilbert distance), all in `u128` so an IPv6 range works the same. `O(facts)`.
    #[must_use]
    pub fn build(range: Cidr, facts: &HashMap<IpAddr, AddressFacts>, max_order: u32) -> MapGrid {
        let host_bits = range.host_bits();
        let order = max_order.min(host_bits / 2);
        let cells = 1u64 << (2 * order);
        let shift = host_bits - 2 * order; // host bits below one cell
        let block = if shift >= 128 { u128::MAX } else { 1u128 << shift };

        let mut used = vec![0u32; cells as usize];
        for addr in facts.keys() {
            if let Some(off) = range.offset_of(*addr) {
                let d = if shift >= 128 { 0 } else { (off >> shift).min(u128::from(cells) - 1) as usize };
                used[d] += 1;
            }
        }
        let max_used = used.iter().copied().max().unwrap_or(0);
        MapGrid { range, order, block, used, sparse: !range.is_enumerable(), max_used }
    }

    /// Grid side length (`2^order`).
    #[must_use]
    pub fn side(&self) -> usize {
        1usize << self.order
    }

    /// Number of cells (`4^order`).
    #[must_use]
    pub fn cells(&self) -> usize {
        1usize << (2 * self.order)
    }

    /// The heat value of cell `d` in `[0, 1]`.
    ///
    /// For an enumerable range this is **absolute** occupancy (used addresses over the
    /// cell's `block`) — "how full is this block". For a sparse IPv6 range, where a cell
    /// spans up to `2^128` addresses and absolute occupancy is always ≈0, it is instead
    /// **relative** density on a log scale (busiest cell = 1), so the map shows *where*
    /// allocations cluster rather than a uniform near-empty field.
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
            (f64::from(used) / self.block as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// The grid position `(x, y)` of cell `d` (its Hilbert placement).
    #[must_use]
    pub fn cell_xy(&self, d: usize) -> (u32, u32) {
        hilbert_d2xy(self.order, d as u64)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(addr: &str) -> AddressFacts {
        AddressFacts { addr: addr.parse().unwrap(), netbox: None, ptr: Some("x.".into()), live: false }
    }

    #[test]
    fn ipv6_map_buckets_and_uses_relative_density() {
        // A /32 is far too big for absolute occupancy; the heat must be *relative* so a
        // busy cell stands out. Two clusters land in two different Hilbert cells.
        let range = Cidr::parse("2001:db8::/32").unwrap();
        let facts: HashMap<_, _> =
            [fact("2001:db8::1"), fact("2001:db8::2"), fact("2001:db8::3"), fact("2001:db8:ffff::1")]
                .into_iter()
                .map(|f| (f.addr, f))
                .collect();
        let g = MapGrid::build(range, &facts, 4);
        assert_eq!(g.used.iter().sum::<u32>(), 4); // all four bucketed
        assert_eq!(g.used.iter().filter(|&&u| u > 0).count(), 2); // in two cells
        let heats: Vec<f32> = (0..g.cells()).map(|d| g.fraction(d)).collect();
        let max = heats.iter().copied().fold(0.0_f32, f32::max);
        assert!((max - 1.0).abs() < 1e-6, "busiest cell should be full heat");
        // The lone-host cell is warm but not maxed (relative log scale).
        assert!(heats.iter().any(|&h| h > 0.0 && h < 0.99));
    }

    #[test]
    fn hilbert_order1_is_the_u_shape() {
        // The base order-1 curve: (0,0)→(0,1)→(1,1)→(1,0).
        assert_eq!(hilbert_d2xy(1, 0), (0, 0));
        assert_eq!(hilbert_d2xy(1, 1), (0, 1));
        assert_eq!(hilbert_d2xy(1, 2), (1, 1));
        assert_eq!(hilbert_d2xy(1, 3), (1, 0));
    }

    #[test]
    fn hilbert_is_a_bijection_and_contiguous() {
        for order in 1..=5u32 {
            let n = 1u32 << order;
            let cells = 1u64 << (2 * order);
            let mut seen = vec![false; cells as usize];
            let mut prev: Option<(u32, u32)> = None;
            for d in 0..cells {
                let (x, y) = hilbert_d2xy(order, d);
                assert!(x < n && y < n);
                // Round-trips.
                assert_eq!(hilbert_xy2d(order, x, y), d);
                // Every cell hit exactly once.
                let i = (y * n + x) as usize;
                assert!(!seen[i]);
                seen[i] = true;
                // Consecutive distances are adjacent (Manhattan distance 1).
                if let Some((px, py)) = prev {
                    let dist = px.abs_diff(x) + py.abs_diff(y);
                    assert_eq!(dist, 1, "d={d} jumped");
                }
                prev = Some((x, y));
            }
            assert!(seen.into_iter().all(|b| b));
        }
    }

    #[test]
    fn a_square_region_is_a_clean_cidr() {
        // The whole /8 at order 4 → cells are /16 blocks; the first cell is 10.0.0.0/16.
        let range = Cidr::parse("10.0.0.0/8").unwrap();
        let g = MapGrid::build(range, &HashMap::new(), 4);
        assert_eq!(g.order, 4); // host_bits 24, capped by max_order 4
        assert_eq!(g.block, 65_536); // 2^16
        let c0 = block_cidr(g.range, g.order, 0);
        assert_eq!(c0.prefix_len, 16);
        assert_eq!(c0.base, "10.0.0.0".parse::<IpAddr>().unwrap());
        // Cell 1 (next Hilbert step) is the adjacent /16.
        assert_eq!(block_cidr(g.range, g.order, 1).base, "10.1.0.0".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn buckets_facts_into_hilbert_cells() {
        let range = Cidr::parse("10.0.0.0/8").unwrap();
        let facts: HashMap<_, _> = [fact("10.0.0.9"), fact("10.1.2.3")]
            .into_iter()
            .map(|f| (f.addr, f))
            .collect();
        let g = MapGrid::build(range, &facts, 4); // /16 cells
        // 10.0.x → cell 0 (offset < 65536); 10.1.x → cell 1.
        assert_eq!(g.used[0], 1);
        assert_eq!(g.used[1], 1);
        assert_eq!(g.used.iter().sum::<u32>(), 2);
    }
}
