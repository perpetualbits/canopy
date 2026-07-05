// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The IP-map view: the range laid on a Hilbert curve as a grid of little squares,
//! each a `/(prefix + 2·order)` block coloured by how full it is. Built from
//! [`crate::map::MapGrid`] each frame (`O(facts)`), so a `/8` maps as cheaply as a
//! `/24`. The legend labels the block structure — the covered CIDR and the per-cell
//! subnet size — not linear x/y ticks, which a Hilbert layout has no use for.
//!
//! Each cell **draws its segment of the actual Hilbert curve** with rounded box-drawing
//! glyphs (`─│╭╮╰╯`), so the serpentine path — which cell follows which — is visible rather
//! than left to the imagination. Occupancy is the cell **background**, per
//! [`DensityStyle`](super::app::DensityStyle):
//! - **Heatmap** (default) — a **logarithmic** ramp, near-black = empty → deep red = barely
//!   used → white = full, with no blue; because almost every block is sparse, the log scale
//!   spreads the low end across the reds/oranges and reserves white for a genuinely full block.
//! - **Shade** — a monochrome grey ramp, for low-colour terminals.
//!
//! The curve line sits on top in a contrasting colour. `s` toggles the two styles. A
//! highlighted cursor moves over the grid (`hjkl`); `Enter` zooms into the cell under it —
//! always a clean subnet — and `Backspace` zooms back out, so a few steps take a `/8` down
//! to a `/24` the table and tree resolve to single addresses.

use std::collections::HashMap;
use std::net::IpAddr;

use mullion::style::{Color, Style};
use mullion::{Buffer, Rect};

use super::app::App;
use super::draw::{btxt, keyhints};
use super::palette::{Knobs, Scheme, KNOBS};
use super::theme::{s_accent, s_dim, s_sel, s_title};
use crate::map::MapGrid;
use crate::reconcile::{self, AddressFacts, Cidr, Subnet};

/// A grid direction from one cell to an adjacent one.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Dir {
    L,
    R,
    U,
    D,
}

/// The direction from grid cell `a` to an adjacent cell `b` (`None` if not 4-adjacent).
fn dir_between(a: (u32, u32), b: (u32, u32)) -> Option<Dir> {
    match (i64::from(b.0) - i64::from(a.0), i64::from(b.1) - i64::from(a.1)) {
        (1, 0) => Some(Dir::R),
        (-1, 0) => Some(Dir::L),
        (0, 1) => Some(Dir::D),
        (0, -1) => Some(Dir::U),
        _ => None,
    }
}

/// The rounded box-drawing glyph for the Hilbert curve through a cell, from the ports toward
/// its previous and next cell on the curve — plus whether the segment continues to the
/// **right** (so the 2-wide cell's spacer is drawn as `─` and the line stays unbroken).
///
/// A cell has two ports (the curve enters and leaves), one at the curve's endpoints, or none
/// for a lone order-0 cell. The glyph joins them: `─│` straight, `╭╮╰╯` for a turn.
fn curve_glyph(a: Option<Dir>, b: Option<Dir>) -> (char, bool) {
    let has = |d: Dir| a == Some(d) || b == Some(d);
    let (l, r, u, dn) = (has(Dir::L), has(Dir::R), has(Dir::U), has(Dir::D));
    let ch = if l && r {
        '─'
    } else if u && dn {
        '│'
    } else if r && u {
        '╰'
    } else if l && u {
        '╯'
    } else if r && dn {
        '╭'
    } else if l && dn {
        '╮'
    } else if l || r {
        '─' // single horizontal port (an endpoint of the curve)
    } else if u || dn {
        '│' // single vertical port
    } else {
        '·' // a lone cell (order 0)
    };
    (ch, r)
}

/// Paint one map cell at `(x, y)`: the Hilbert-curve `glyph` in column `x` on background
/// `bg`, foreground `fg`, then a spacer in `x + 1` — a `─` when the curve continues right so
/// the line is unbroken, otherwise blank. The colours come from the active
/// [`Scheme`](super::palette::Scheme); `selected` paints both columns in the cursor style.
fn paint_cell(buf: &mut Buffer, x: u16, y: u16, bg: Color, fg: Color, selected: bool, curve: (char, bool)) {
    let (glyph, connects_right) = curve;
    let cell = if selected { s_sel() } else { Style::default().fg(fg).bg(bg) };
    buf.set_char(x, y, glyph, cell);
    buf.set_char(x + 1, y, if connects_right { '─' } else { ' ' }, cell);
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

/// Draw the palette key at `(x, y)`: the scheme name, an `empty → full` swatch strip
/// generated from the scheme itself (so it is self-documenting under any knob setting), and
/// the currently-selected knob and its value.
fn draw_legend_key(buf: &mut Buffer, x: u16, y: u16, scheme: Scheme, knobs: &Knobs, active_knob: usize) {
    let cx = buf.set_string(x, y, &format!("scheme: {} [p] · ", scheme.name()), s_dim());
    // Background swatches sampled from the scheme across the occupancy range (empty → full).
    let mut sx = cx;
    for k in 0..12u16 {
        let frac = if k == 0 { 0.0 } else { 10f32.powf((f32::from(k) / 11.0 - 1.0) * knobs.decades) };
        let (bg, _) = scheme.paint(frac, 0.5, knobs);
        buf.set_char(sx + k, y, ' ', Style::default().bg(bg));
    }
    sx += 12;
    // The active knob + value (selected with [ ], adjusted with , .).
    let (name, ..) = KNOBS[active_knob];
    buf.set_string(sx, y, &format!("  knob [{}] {} = {:.2}  [,.]", active_knob, name, knobs.get(active_knob)), s_dim());
}

/// A short, comma-separated list of the hostnames inside `sub` — what lives in the
/// block under the cursor. Shows up to `max` names, then `+N` for the rest; `—` when
/// the block is empty. Names come from the reconciled facts (PTR or NetBox name).
fn names_in(facts: &HashMap<IpAddr, AddressFacts>, sub: Cidr, max: usize) -> String {
    let mut names: Vec<String> = facts
        .values()
        .filter(|f| sub.contains(f.addr))
        .filter_map(|f| reconcile::row_from_facts(f).name)
        .collect();
    if names.is_empty() {
        return "—".to_string();
    }
    names.sort();
    let extra = names.len().saturating_sub(max);
    let mut shown = names.into_iter().take(max).collect::<Vec<_>>().join(", ");
    if extra > 0 {
        shown.push_str(&format!(", +{extra}"));
    }
    shown
}

/// Clip `text` to at most `w` columns (so an info line never overruns the screen).
fn clip(text: &str, w: u16) -> String {
    text.chars().take(w as usize).collect()
}

/// Paint the map view for the current [`App`] state.
pub fn screen(buf: &mut Buffer, app: &mut App) {
    let full = buf.area;
    if full.width < 26 || full.height < 8 {
        return;
    }

    // ── frame (title + data badge in the border) ──
    let title = format!("canopy — map: {}/{}", app.range.base, app.range.prefix_len);
    let prog = app.progress.as_ref().map(|(f, l)| (*f, l.as_str()));
    let area = super::draw::frame(buf, full, &title, s_title(), Some(super::draw::data_badge(app)), prog, &app.heartbeat());

    // Layout: three header rows — legend, cursor info, scope — ABOVE the Hilbert square, so
    // the "what am I looking at" lines lead; then the grid; then the footer on the last row.
    let legend_y = area.y;
    let info_y = area.y + 1;
    let scope_y = area.y + 2;
    let foot_y = area.y + area.height - 1;
    let body = Rect::new(area.x, area.y + 3, area.width, area.height.saturating_sub(4));

    let grid = MapGrid::build(app.range, &app.facts, fit_order(body));
    let side = grid.side();
    let used_total: u32 = grid.used.iter().sum();
    let cell_prefix = app.range.prefix_len + 2 * grid.order as u8;

    // Sync the app's cursor state to this frame's grid: the order sets what `Enter`
    // zooms into, and a shrunk terminal may need the cursor clamped back in-bounds.
    app.map_order = grid.order;
    let last = (side as u32).saturating_sub(1);
    app.map_cur = (app.map_cur.0.min(last), app.map_cur.1.min(last));

    // Row 0 — block structure + density key (Hilbert has no meaningful linear x/y axis).
    let head = format!(
        "Hilbert · {side}×{side} · cell = /{cell_prefix} ({} addrs) · {used_total} used / {} total   ",
        grid.block,
        grid.range.block_len()
    );
    btxt(buf, area.x, legend_y, &head, s_dim());
    draw_legend_key(buf, area.x + head.chars().count() as u16, legend_y, app.scheme, &app.knobs, app.active_knob);

    // Rows 1–2 — the block under the cursor (CIDR, span, occupancy, hostnames) and the
    // scope breadcrumb + real NetBox subnet. When the grid is one cell, that block is the
    // whole current scope.
    let zoomable = app.cursor_subnet().is_some();
    let (cell, used, block) = match app.cursor_subnet() {
        Some(sub) => {
            let d = crate::map::hilbert_xy2d(grid.order, app.map_cur.0, app.map_cur.1) as usize;
            (sub, grid.used.get(d).copied().unwrap_or(0), grid.block)
        }
        None => (app.range, used_total, grid.range.block_len()),
    };
    // For a sparse (huge) block the "/block" denominator is astronomically large and
    // unhelpful, so show just the used count; for an enumerable block show used/total.
    let occ = if cell.is_enumerable() { format!("{used}/{block} used") } else { format!("{used} used") };
    let info = format!(
        "▸ {}/{}   {} – {}   {occ}   {}",
        cell.base,
        cell.prefix_len,
        cell.base,
        cell.last(),
        names_in(&app.facts, cell, 3),
    );
    btxt(buf, area.x, info_y, &clip(&info, area.width), s_accent());

    let crumb = app
        .scope_chain()
        .iter()
        .map(|c| format!("{}/{}", c.base, c.prefix_len))
        .collect::<Vec<_>>()
        .join(" › ");
    let subnet_txt = match Subnet::most_specific(&app.subnets, cell.base) {
        Some(s) if !s.name.is_empty() => format!("   ·   subnet: {}/{} ({})", s.cidr.base, s.cidr.prefix_len, s.name),
        Some(s) => format!("   ·   subnet: {}/{}", s.cidr.base, s.cidr.prefix_len),
        None => String::new(),
    };
    btxt(
        buf,
        area.x,
        scope_y,
        &clip(&format!("scope: {crumb}{subnet_txt}   ·   the line is the Hilbert curve · bg = occupancy"), area.width),
        s_dim(),
    );

    // The Hilbert grid: each cell draws its segment of the actual curve (rounded box glyphs)
    // over a background coloured by occupancy, so the serpentine path is visible directly.
    let total = grid.cells();
    for d in 0..total {
        let cur = grid.cell_xy(d);
        let (gx, gy) = cur;
        let selected = (gx, gy) == app.map_cur;
        let x = body.x + (gx as u16) * 2;
        let y = body.y + gy as u16;
        if x + 1 < body.x + body.width && y < body.y + body.height {
            // The ports toward the previous and next cell give the glyph; the active scheme
            // gives the (background, curve) colours from occupancy and curve position.
            let prev = (d > 0).then(|| grid.cell_xy(d - 1)).and_then(|p| dir_between(cur, p));
            let next = (d + 1 < total).then(|| grid.cell_xy(d + 1)).and_then(|n| dir_between(cur, n));
            let pos = if total > 1 { d as f32 / (total - 1) as f32 } else { 0.0 };
            let (bg, fg) = app.scheme.paint(grid.fraction(d), pos, &app.knobs);
            paint_cell(buf, x, y, bg, fg, selected, curve_glyph(prev, next));
        }
    }

    // Footer key hints (zoom only offered while there's a finer subnet to reach).
    let hints: &[(&str, &str)] = if zoomable {
        &[("hjkl", "move"), ("↵", "in"), ("Bksp", "out"), ("p", "palette"), ("[ ]", "knob"), (", .", "tune"), ("Tab", "table"), ("q", "quit")]
    } else {
        &[("p", "palette"), ("[ ]", "knob"), (", .", "tune"), ("Tab", "table"), ("q", "quit")]
    };
    keyhints(buf, area.x, foot_y, area.width, hints);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_glyph_joins_the_ports() {
        // Straights, turns, endpoints, and the lone cell.
        assert_eq!(curve_glyph(Some(Dir::L), Some(Dir::R)).0, '─');
        assert_eq!(curve_glyph(Some(Dir::U), Some(Dir::D)).0, '│');
        assert_eq!(curve_glyph(Some(Dir::R), Some(Dir::D)).0, '╭');
        assert_eq!(curve_glyph(Some(Dir::L), Some(Dir::D)).0, '╮');
        assert_eq!(curve_glyph(Some(Dir::R), Some(Dir::U)).0, '╰');
        assert_eq!(curve_glyph(Some(Dir::L), Some(Dir::U)).0, '╯');
        assert_eq!(curve_glyph(None, Some(Dir::R)).0, '─'); // endpoint
        assert_eq!(curve_glyph(None, None).0, '·'); // order-0 lone cell
        // The connects-right flag (drives the horizontal spacer) is set iff a port faces right.
        assert!(curve_glyph(Some(Dir::L), Some(Dir::R)).1);
        assert!(!curve_glyph(Some(Dir::L), Some(Dir::U)).1);
    }

    #[test]
    fn dir_between_reads_grid_adjacency() {
        assert_eq!(dir_between((2, 2), (3, 2)), Some(Dir::R));
        assert_eq!(dir_between((2, 2), (2, 1)), Some(Dir::U));
        assert_eq!(dir_between((2, 2), (4, 2)), None); // not 4-adjacent
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
            app.on_key(KeyCode::Char('s')); // flip Heatmap ↔ Shade and render again
        }
    }
}
