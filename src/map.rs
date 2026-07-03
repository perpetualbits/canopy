// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP map: lay an address range onto a **Hilbert curve** so that every
//! axis-aligned power-of-two square is a contiguous CIDR — a zoomed square is always
//! a clean subnet — while consecutive addresses stay spatially adjacent (the curve
//! never jumps, unlike Z-order), so a contiguous allocation reads as one blob.
//!
//! A range is mapped over its **full** `2^(32−prefix)` address block (not the usable
//! hosts, whose count isn't a power of two) so the quadrant tiling is exact. The grid
//! is `2^order × 2^order`; each cell covers a `/(prefix + 2·order)` block, shaded by
//! how used it is. Building is `O(facts)`. Rendering lives in `tui::map`.

use std::collections::HashMap;
use std::net::Ipv4Addr;

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
/// A cell holds `block = block_len / 4^order` addresses, so cell `d` starts at offset
/// `d · block` from the range's network address; its prefix grows by `2·order` bits.
#[must_use]
pub fn block_cidr(range: Cidr, order: u32, d: u64) -> Cidr {
    let block = range.block_len() >> (2 * order);
    Cidr { base: range.address_at_offset(d * block), prefix_len: range.prefix_len + 2 * order as u8 }
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
    /// Addresses per cell — `2^(host_bits − 2·order)`, a power of two.
    pub block: u64,
    /// Used-address count per cell, indexed by **Hilbert distance** `d`.
    pub used: Vec<u32>,
}

impl MapGrid {
    /// Build the map for `range` from the (bounded) `facts`, at the largest order up
    /// to `max_order` that still gives at least one address per cell.
    ///
    /// The order is capped at `host_bits / 2` so `2·order ≤ host_bits` (a cell is a
    /// real, ≥1-address CIDR). Each known address is bucketed into cell
    /// `offset / block` (its Hilbert distance). `O(facts)`.
    #[must_use]
    pub fn build(range: Cidr, facts: &HashMap<Ipv4Addr, AddressFacts>, max_order: u32) -> MapGrid {
        let host_bits = 32 - u32::from(range.prefix_len);
        let order = max_order.min(host_bits / 2);
        let cells = 1u64 << (2 * order);
        let block = (range.block_len() / cells).max(1);

        let mut used = vec![0u32; cells as usize];
        for addr in facts.keys() {
            if let Some(off) = range.offset_of(*addr) {
                let d = (off / block).min(cells - 1) as usize;
                used[d] += 1;
            }
        }
        MapGrid { range, order, block, used }
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

    /// The used fraction of cell `d` in `[0, 1]` (used addresses over `block`).
    #[must_use]
    pub fn fraction(&self, d: usize) -> f32 {
        (f64::from(self.used[d]) / self.block as f64).clamp(0.0, 1.0) as f32
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
        assert_eq!(c0.base, "10.0.0.0".parse::<Ipv4Addr>().unwrap());
        // Cell 1 (next Hilbert step) is the adjacent /16.
        assert_eq!(block_cidr(g.range, g.order, 1).base, "10.1.0.0".parse::<Ipv4Addr>().unwrap());
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
