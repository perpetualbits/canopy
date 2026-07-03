// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP-map view: the address range as a grid of little squares, each a block of
//! addresses shaded by how used it is (`·` free, `░▒▓█` increasingly used). Built
//! from [`crate::map::MapGrid`] each frame (`O(facts)`), so a `/8` maps as cheaply
//! as a `/24`. First cut — the axis legend and zoom are still to be designed.

use mullion::style::Style;
use mullion::{Buffer, Rect};

use super::app::App;
use super::draw::{btxt, keyhints};
use super::theme::{s_accent, s_dim, s_ok, s_title, s_warn};
use crate::map::MapGrid;

/// Glyph + style for a cell of used-fraction `f`: `·` (dim) when empty, otherwise a
/// shade block `░▒▓█` deepening with density, in the accent colour. (The colour
/// scheme is a legend-design open question — see the round of discussion.)
fn cell_glyph(f: f32) -> (&'static str, Style) {
    if f <= 0.0 {
        return ("·", s_dim());
    }
    let level = ((f * 4.0).ceil() as usize).clamp(1, 4);
    let glyph = ["░", "▒", "▓", "█"][level - 1];
    (glyph, s_accent())
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

    // ── body grid: each cell is 2 columns wide for a squarer aspect ──
    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(3));
    let cols = (body.width / 2).max(1) as usize;
    let rows = body.height.max(1) as usize;
    let grid = MapGrid::build(app.range, &app.facts, cols, rows);
    let used_total: u32 = grid.used.iter().sum();

    // Sub-header: what one cell means, the address span, and a shade key. The exact
    // axis-labelling is the legend-design open question; this is a first cut.
    let first = grid.cell_start(app.range, 0);
    let last = grid.cell_start(app.range, cols * rows - 1);
    btxt(
        buf,
        area.x,
        area.y + 1,
        &format!(
            "{cols}×{rows} · ~{} addrs/cell · {first}→{last} · {used_total} used / {} total   [· free  ░▒▓█ used]",
            grid.block, grid.total
        ),
        s_dim(),
    );

    for r in 0..rows {
        for c in 0..cols {
            let i = r * cols + c;
            let (glyph, style) = cell_glyph(grid.fraction(i));
            let x = body.x + (c * 2) as u16;
            let y = body.y + r as u16;
            buf.set_string(x, y, glyph, style);
            buf.set_string(x + 1, y, glyph, style);
        }
    }

    // ── footer ──
    keyhints(
        buf,
        area.x,
        area.y + area.height - 1,
        area.width,
        &[("Tab", "table"), ("q", "quit")],
    );
}
