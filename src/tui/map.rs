// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP-map view: the range laid on a Hilbert curve as a grid of little squares,
//! each a `/(prefix + 2·order)` block shaded by how used it is (`·` free, `░▒▓█`
//! used). Built from [`crate::map::MapGrid`] each frame (`O(facts)`), so a `/8` maps
//! as cheaply as a `/24`. The legend labels the block structure — the covered CIDR
//! and the per-cell subnet size — not linear x/y ticks, which a Hilbert layout has no
//! use for. A highlighted cursor moves over the grid (`hjkl`); `Enter` zooms into the
//! cell under it — always a clean subnet — and `Backspace` zooms back out, so a few
//! steps take a `/8` down to a `/24` the table and tree resolve to single addresses.

use mullion::style::Style;
use mullion::{Buffer, Rect};

use super::app::App;
use super::draw::{btxt, keyhints};
use super::theme::{s_accent, s_dim, s_ok, s_sel, s_title, s_warn};
use crate::map::MapGrid;

/// Glyph + style for a cell of used-fraction `f`: `·` (dim) when empty, otherwise a
/// shade block `░▒▓█` deepening with density, in the accent colour.
fn cell_glyph(f: f32) -> (&'static str, Style) {
    if f <= 0.0 {
        return ("·", s_dim());
    }
    let level = ((f * 4.0).ceil() as usize).clamp(1, 4);
    let glyph = ["░", "▒", "▓", "█"][level - 1];
    (glyph, s_accent())
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
    btxt(
        buf,
        area.x,
        area.y + 1,
        &format!(
            "Hilbert · {side}×{side} · cell = /{cell_prefix} ({} addrs) · {used_total} used / {} total   [· free  ░▒▓█ used]",
            grid.block, grid.range.block_len()
        ),
        s_dim(),
    );

    for d in 0..grid.cells() {
        let (gx, gy) = grid.cell_xy(d);
        let (glyph, style) = cell_glyph(grid.fraction(d));
        let selected = (gx, gy) == app.map_cur;
        let style = if selected { s_sel() } else { style };
        let x = body.x + (gx as u16) * 2; // 2-wide cells for a squarer aspect
        let y = body.y + gy as u16;
        if x + 1 < body.x + body.width && y < body.y + body.height {
            buf.set_string(x, y, glyph, style);
            buf.set_string(x + 1, y, glyph, style);
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
            &[("hjkl", "move"), ("↵", "zoom in"), ("Bksp", "out"), ("Tab", "table"), ("q", "quit")],
        );
    } else {
        keyhints(buf, area.x, area.y + area.height - 1, area.width, &[("Tab", "table"), ("q", "quit")]);
    }
}
