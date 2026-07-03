// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP-map view: the range laid on a Hilbert curve as a grid of little squares,
//! each a `/(prefix + 2·order)` block shaded by how used it is. Built from
//! [`crate::map::MapGrid`] each frame (`O(facts)`), so a `/8` maps as cheaply as a
//! `/24`. The legend labels the block structure — the covered CIDR and the per-cell
//! subnet size — not linear x/y ticks, which a Hilbert layout has no use for.
//!
//! Free cells are a calm dim `·`; used cells shade by
//! [`DensityStyle`](super::app::DensityStyle) — either a static block `░▒▓█`, or a
//! scrolling `▪`/`▫` **bitstream** where the share of solid squares equals the cell's
//! occupancy, each cell its own golden-angle hue via [`mullion::FlowStyle`] (the surf
//! field from aerie's `spiral_stress`). `s` toggles the two.
//!
//! A highlighted cursor moves over the grid (`hjkl`); `Enter` zooms into the cell
//! under it — always a clean subnet — and `Backspace` zooms back out, so a few steps
//! take a `/8` down to a `/24` the table and tree resolve to single addresses.

use mullion::style::Style;
use mullion::{Buffer, FlowStyle, Rect};

use super::app::{App, DensityStyle};
use super::draw::{btxt, keyhints};
use super::theme::{s_accent, s_dim, s_ok, s_sel, s_title, s_warn};
use crate::map::MapGrid;

/// Bit-cells the map's bitstream scrolls past per second — its scroll speed
/// (aerie's `spiral_stress` uses 5.0 on its border gaps; a touch slower reads
/// calmer on a dense grid).
const BIT_SPEED: f32 = 4.0;

/// Glyph + style for a *shade*-style cell of used-fraction `f`: `·` (dim) when empty,
/// otherwise a shade block `░▒▓█` deepening with density, in the accent colour.
fn cell_glyph(f: f32) -> (&'static str, Style) {
    if f <= 0.0 {
        return ("·", s_dim());
    }
    let level = ((f * 4.0).ceil() as usize).clamp(1, 4);
    let glyph = ["░", "▒", "▓", "█"][level - 1];
    (glyph, s_accent())
}

/// Hash a `(cell, bit-index)` pair to a value in `[0, 1)` — the fixed per-cell bit
/// pattern the scrolling window reads. It is deterministic, so a solid bit travels
/// smoothly along as the window advances. (A standard integer-mix hash — SplitMix-ish.)
fn hash01(cell: u64, k: i64) -> f32 {
    let mut x = cell.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (k as u64).wrapping_mul(0xD1B5_4A32_D192_ED03);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    // Top 24 bits → [0, 1); 2^24 = 16_777_216 keeps full f32 mantissa precision.
    (x >> 40) as f32 / 16_777_216.0
}

/// Is the bit at continuous stream position `s` set, for a cell whose used fraction
/// is `frac`? The fixed per-cell pattern (see [`hash01`]) has about `frac` of its
/// bit-cells solid; flooring `s` picks one, so advancing the window (via time) scrolls
/// the pattern. `frac = 0` → never set (all `▫`), `frac = 1` → always (all `▪`). This
/// keeps density honest: the share of solid squares equals the cell's occupancy.
fn stream_bit(cell: u64, s: f32, frac: f32) -> bool {
    frac > 0.0 && hash01(cell, s.floor() as i64) < frac
}

/// Paint one map cell (2 columns wide) at buffer position `(x, y)`.
///
/// Free cells (`frac == 0`) stay a calm dim `·` in either style, so the map still
/// reads "where is stuff" at a glance. Used cells render per [`DensityStyle`]: a
/// static shade block, or the scrolling `▪`/`▫` bitstream. `selected` paints the
/// whole cell in the cursor style so the highlight always wins.
#[allow(clippy::too_many_arguments)]
fn paint_cell(buf: &mut Buffer, x: u16, y: u16, d: usize, side: usize, frac: f32, style: DensityStyle, t: f32, selected: bool) {
    if frac <= 0.0 {
        let s = if selected { s_sel() } else { s_dim() };
        buf.set_char(x, y, '·', s);
        buf.set_char(x + 1, y, '·', s);
        return;
    }
    match style {
        DensityStyle::Shade => {
            let (glyph, base) = cell_glyph(frac);
            let ch = glyph.chars().next().unwrap_or('█');
            let s = if selected { s_sel() } else { base };
            buf.set_char(x, y, ch, s);
            buf.set_char(x + 1, y, ch, s);
        }
        DensityStyle::Bitstream => {
            // Each cell is its own data channel: a golden-angle hue band, alternating
            // scroll direction, streaming its occupancy as bits. Set bits glow.
            let cell = d as u64;
            let dir = if cell % 2 == 0 { 1.0 } else { -1.0 };
            let flow = FlowStyle { band: d, speed: 0.55, sweep: 90.0, direction: dir };
            for j in 0..2u16 {
                let coord = f32::from(x) + f32::from(j);
                let bit = stream_bit(cell, coord - t * BIT_SPEED * dir, frac);
                let ch = if bit { '▪' } else { '▫' }; // solid square = set bit, open = clear
                let pos = coord / (2.0 * side as f32); // hue sweeps across the row
                let s = if selected { s_sel() } else { flow.color(pos, t, bit) };
                buf.set_char(x + j, y, ch, s);
            }
        }
    }
}

/// The largest Hilbert order whose `2^order × 2^order` grid of 2-wide cells fits in
/// `body` — `floor(log2(min(width/2, height)))`.
fn fit_order(body: Rect) -> u32 {
    let side_max = (body.width / 2).min(body.height);
    if side_max < 1 {
        0
    } else {
        u32::BITS - 1 - u32::from(side_max).leading_zeros()
    }
}

/// Paint the map view for the current [`App`] state.
pub fn screen(buf: &mut Buffer, app: &mut App) {
    let area = buf.area;
    if area.width < 24 || area.height < 6 {
        return;
    }

    // ── header ──
    let title = format!("netpush — map: {}/{}", app.range.base, app.range.prefix_len);
    btxt(buf, area.x, area.y, &title, s_title());
    let (data, dstyle) = if app.live { ("LIVE", s_ok()) } else { ("DEMO", s_warn()) };
    btxt(buf, area.x + title.chars().count() as u16 + 2, area.y, data, dstyle);

    // ── grid ──
    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(3));
    let grid = MapGrid::build(app.range, &app.facts, fit_order(body));
    let side = grid.side();
    let used_total: u32 = grid.used.iter().sum();
    let cell_prefix = app.range.prefix_len + 2 * grid.order as u8;

    // Sync the app's cursor state to this frame's grid: the order sets what `Enter`
    // zooms into, and a shrunk terminal may need the cursor clamped back in-bounds.
    app.map_order = grid.order;
    let last = (side as u32).saturating_sub(1);
    app.map_cur = (app.map_cur.0.min(last), app.map_cur.1.min(last));

    // Legend: block structure, not linear ticks (Hilbert has no meaningful x/y axis).
    let key = match app.density {
        DensityStyle::Bitstream => "[· free  ▫▪ used bitstream]",
        DensityStyle::Shade => "[· free  ░▒▓█ used]",
    };
    btxt(
        buf,
        area.x,
        area.y + 1,
        &format!(
            "Hilbert · {side}×{side} · cell = /{cell_prefix} ({} addrs) · {used_total} used / {} total   {key}",
            grid.block, grid.range.block_len()
        ),
        s_dim(),
    );

    let t = app.anim_t();
    for d in 0..grid.cells() {
        let (gx, gy) = grid.cell_xy(d);
        let selected = (gx, gy) == app.map_cur;
        let x = body.x + (gx as u16) * 2; // 2-wide cells for a squarer aspect
        let y = body.y + gy as u16;
        if x + 1 < body.x + body.width && y < body.y + body.height {
            paint_cell(buf, x, y, d, side, grid.fraction(d), app.density, t, selected);
        }
    }

    // ── footer ──
    // Show the block the cursor sits over — what `Enter` would zoom into — and the
    // key hints (zoom only offered while there's a finer subnet to reach).
    if let Some(sub) = app.cursor_subnet() {
        let depth = app.zoom_depth();
        let crumb = if depth > 0 { format!("  (depth {depth}, Bksp: out)") } else { String::new() };
        btxt(buf, area.x, body.y + body.height, &format!("▸ {}/{}{crumb}", sub.base, sub.prefix_len), s_accent());
        keyhints(
            buf,
            area.x,
            area.y + area.height - 1,
            area.width,
            &[("hjkl", "move"), ("↵", "zoom in"), ("Bksp", "out"), ("s", "style"), ("Tab", "table"), ("q", "quit")],
        );
    } else {
        keyhints(
            buf,
            area.x,
            area.y + area.height - 1,
            area.width,
            &[("s", "style"), ("Tab", "table"), ("q", "quit")],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash01_stays_in_the_unit_interval() {
        for cell in 0..64u64 {
            for k in -50..50i64 {
                let r = hash01(cell, k);
                assert!((0.0..1.0).contains(&r), "hash01({cell},{k}) = {r} out of [0,1)");
            }
        }
    }

    #[test]
    fn stream_bit_honours_the_fraction_extremes() {
        // An empty cell is never solid; a full cell always is — whatever the phase.
        for s in [-3.5, 0.0, 2.2, 17.9, 100.0] {
            assert!(!stream_bit(7, s, 0.0), "frac 0 must be all ▫");
            assert!(stream_bit(7, s, 1.0), "frac 1 must be all ▪");
        }
    }

    #[test]
    fn stream_bit_solid_share_tracks_occupancy() {
        // The whole point of the density-honest bitstream: over the stream, the
        // share of solid squares should be close to the cell's used fraction.
        for &frac in &[0.25_f32, 0.5, 0.75] {
            let cell = 42;
            let n = 4000;
            let set = (0..n).filter(|&k| stream_bit(cell, f32::from(k as u16) + 0.5, frac)).count();
            let share = set as f32 / n as f32;
            assert!((share - frac).abs() < 0.05, "frac {frac}: solid share {share} drifted too far");
        }
    }

    #[test]
    fn renders_both_styles_without_panicking() {
        use crate::fixture;
        use mullion::{Buffer, KeyCode, Rect};

        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, crate::config::Config::default());
        app.view = super::super::app::View::Map;
        for _ in 0..2 {
            for (w, h) in [(120u16, 50u16), (80, 24), (40, 10), (24, 6)] {
                let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
                screen(&mut buf, &mut app);
            }
            app.on_key(KeyCode::Char('s')); // flip Bitstream ↔ Shade and render again
        }
    }
}
