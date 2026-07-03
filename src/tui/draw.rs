// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Buffer drawing: the reconciled-address screen and the small primitives it uses
//! (title, count bar, rows, key-hint footer, scrollbar). Primitives mirror census.

use mullion::border::{draw_box, BorderStyle, Borders, CornerStyle, LineWeight};
use mullion::style::Color;
use mullion::{render_keyhints, render_scrollbar, style::Style, Buffer, Rect, RecordSource, ScrollMetrics, TextCtx};

use super::app::{AllocPhase, App};
use super::theme::{
    s_accent, s_dim, s_err, s_head, s_normal, s_ok, s_sel, s_title, s_warn, status_label, status_style,
};
use crate::reconcile::{AddressStatus, Counts};

/// Write `text` at `(x, y)` in `style`.
pub fn btxt(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    buf.set_string(x, y, text, style);
}

/// Fill `w` cells from `(x, y)` with spaces in `style` — used for the row highlight.
pub fn fill_row(buf: &mut Buffer, x: u16, y: u16, w: u16, style: Style) {
    for cx in x..x + w {
        buf.set_string(cx, y, " ", style);
    }
}

/// Themed key-hint footer via mullion's `render_keyhints`.
pub fn keyhints(buf: &mut Buffer, x: u16, y: u16, w: u16, pairs: &[(&str, &str)]) {
    render_keyhints(buf, Rect::new(x, y, w, 1), pairs, &super::theme::mullion_theme(), TextCtx::LTR);
}

/// Draw a vertical scrollbar in the last column of `area` when `len` rows overflow
/// a `vis`-row window at `offset`; returns the content rect (minus the bar column).
pub fn vscroll(buf: &mut Buffer, area: Rect, offset: usize, len: usize, vis: usize) -> Rect {
    if len <= vis || vis == 0 || area.width < 2 {
        return area;
    }
    let bar = Rect::new(area.x + area.width - 1, area.y, 1, area.height);
    let metrics = ScrollMetrics {
        position: offset as f32 / len as f32,
        extent: vis as f32 / len as f32,
        exact: true,
    };
    render_scrollbar(buf, bar, metrics, s_dim());
    Rect::new(area.x, area.y, area.width - 1, area.height)
}

/// Paint the whole screen for the current [`App`] state.
///
/// Layout (top to bottom): title row, count bar, column header, the scrollable
/// address table, and a key-hint footer. Mutates `app` only to keep the cursor's
/// scroll offset in view for the body height we just measured.
pub fn screen(buf: &mut Buffer, app: &mut App) {
    let area = buf.area;
    if area.width < 24 || area.height < 6 {
        return; // too small to draw anything meaningful
    }

    // ── title row ──
    let mode = app.mode_label();
    let title = format!("netpush — {}/{}", app.range.base, app.range.prefix_len);
    btxt(buf, area.x, area.y, &title, s_title());
    // DEMO / LIVE / LOADING badge just after the title, so the data source is obvious.
    let (data, data_style) = if app.loading {
        ("LOADING…", s_warn())
    } else if app.live {
        ("LIVE", s_ok())
    } else {
        ("DEMO", s_warn())
    };
    let badge_x = area.x + title.chars().count() as u16 + 2;
    btxt(buf, badge_x, area.y, data, data_style);
    // Optional status message after the badge (dim, or red on error).
    if let Some((msg, err)) = &app.status {
        let sx = badge_x + data.chars().count() as u16 + 2;
        let room = area.width.saturating_sub(sx - area.x + 12); // leave space for the mode badge
        if room > 8 {
            let text: String = msg.chars().take(room as usize).collect();
            btxt(buf, sx, area.y, &text, if *err { s_err() } else { s_dim() });
        }
    }
    // Mode badge at the right.
    btxt(
        buf,
        area.x + area.width.saturating_sub(mode.0.len() as u16 + 1),
        area.y,
        mode.0,
        mode.1,
    );

    // ── count bar ──
    count_bar(buf, area.x, area.y + 1, &app.counts);

    // ── column header ──
    let hy = area.y + 2;
    btxt(buf, area.x, hy, "ADDRESS", s_dim());
    btxt(buf, area.x + 16, hy, "STATUS", s_dim());
    btxt(buf, area.x + 34, hy, "NAME", s_dim());

    // ── body ──
    let body = Rect::new(area.x, area.y + 3, area.width, area.height.saturating_sub(4));
    let len = app.total;
    app.page = body.height as usize;
    app.cur.clamp(len);
    app.cur.keep_in_view(len, body.height as usize);

    let content = vscroll(buf, body, app.cur.offset, len, body.height as usize);
    let vis = content.height as usize;
    // Fetch only the visible window from the paginated RangeSource — never the whole
    // (possibly huge) range. `fetch_after(Some(offset-1), vis)` yields [offset, +vis).
    let key = app.cur.offset.checked_sub(1).map(|k| k as u64);
    let window = app.table_source().fetch_after(key, vis);
    for (idx, row) in &window.rows {
        let i = *idx as usize;
        let y = content.y + (i - app.cur.offset) as u16;
        let selected = i == app.cur.cursor;
        if selected {
            fill_row(buf, content.x, y, content.width, s_sel());
        }
        let base = if selected { s_sel() } else { s_normal() };
        let stat = if selected { s_sel() } else { status_style(row.status) };
        let name_style = if selected { s_sel() } else { s_dim() };

        btxt(buf, content.x, y, &format!("{:<15}", row.addr.to_string()), base);
        btxt(buf, content.x + 16, y, status_label(row.status), stat);
        if let Some(n) = &row.name {
            btxt(buf, content.x + 34, y, n, name_style);
        }
    }

    // ── footer ──
    keyhints(
        buf,
        area.x,
        area.y + area.height - 1,
        area.width,
        &[
            ("j/k", "move"),
            ("f", "next free"),
            ("a", "allocate"),
            ("Enter", "inspect"),
            ("L", "live"),
            ("Tab", "graph"),
            ("q", "quit"),
        ],
    );

    // ── overlays (allocate takes precedence over inspect) ──
    if app.alloc.is_some() {
        alloc_overlay(buf, area, app);
    } else if app.detail {
        detail_overlay(buf, area, app);
    }
}

/// The allocate flow overlay: type a name, then review the plan before applying.
fn alloc_overlay(buf: &mut Buffer, area: Rect, app: &App) {
    let Some(flow) = &app.alloc else {
        return;
    };
    match flow.phase {
        AllocPhase::Naming => {
            if area.width < 40 || area.height < 10 {
                return;
            }
            let w = 60u16.min(area.width - 4);
            let h = 7u16;
            let x = area.x + (area.width - w) / 2;
            let y = area.y + (area.height - h) / 2;
            let bgc = Color::Rgb(28, 28, 44);
            for yy in y..y + h {
                fill_row(buf, x, yy, w, Style::default().bg(bgc));
            }
            let bs = BorderStyle { weight: LineWeight::Heavy, corners: CornerStyle::Rounded, style: s_accent() };
            draw_box(buf, Rect::new(x, y, w, h), Borders::ALL, &bs);
            btxt(buf, x + 2, y + 1, &format!("Allocate {}", flow.addr), s_head().bg(bgc));
            btxt(buf, x + 2, y + 3, "name:", s_dim().bg(bgc));
            let line = format!("{}\u{2588}", flow.input); // trailing cursor block
            let line: String = line.chars().take((w - 10) as usize).collect();
            btxt(buf, x + 8, y + 3, &line, s_normal().bg(bgc));
            btxt(buf, x + 2, y + 5, "[Enter] preview   [Esc] cancel", s_dim().bg(bgc));
        }
        AllocPhase::Preview => {
            if area.width < 50 || area.height < 14 {
                return;
            }
            let w = (area.width - 4).min(80);
            let h = (area.height - 4).min(22);
            let x = area.x + (area.width - w) / 2;
            let y = area.y + (area.height - h) / 2;
            let bgc = Color::Rgb(24, 24, 38);
            for yy in y..y + h {
                fill_row(buf, x, yy, w, Style::default().bg(bgc));
            }
            let bs = BorderStyle { weight: LineWeight::Heavy, corners: CornerStyle::Rounded, style: s_accent() };
            draw_box(buf, Rect::new(x, y, w, h), Borders::ALL, &bs);

            let text = flow.plan.as_ref().map(|p| p.preview()).unwrap_or_default();
            let max_lines = (h as usize).saturating_sub(3);
            for (i, l) in text.lines().take(max_lines).enumerate() {
                let l: String = l.chars().take((w - 4) as usize).collect();
                btxt(buf, x + 2, y + 1 + i as u16, &l, s_normal().bg(bgc));
            }
            let hint = if app.applying {
                "applying…"
            } else if app.can_apply() {
                "[y] apply   [Esc] cancel"
            } else {
                "read-only — restart with --write to apply     [Esc] cancel"
            };
            btxt(buf, x + 2, y + h - 2, hint, s_warn().bg(bgc));
        }
    }
}

/// A centred panel showing the selected address's facts from each source and the
/// reason for its verdict — the "why" behind the status.
pub(crate) fn detail_overlay(buf: &mut Buffer, area: Rect, app: &App) {
    if app.total == 0 || area.width < 44 || area.height < 12 {
        return;
    }
    let row = app.row_at(app.cur.cursor.min(app.total - 1));
    let w = 58u16.min(area.width - 4);
    let h = 9u16.min(area.height - 4);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;

    let bgc = Color::Rgb(28, 28, 44);
    for yy in y..y + h {
        fill_row(buf, x, yy, w, Style::default().bg(bgc));
    }
    let box_style = BorderStyle { weight: LineWeight::Heavy, corners: CornerStyle::Rounded, style: s_accent() };
    draw_box(buf, Rect::new(x, y, w, h), Borders::ALL, &box_style);

    let facts = app.facts_for(row.addr);
    let netbox = match facts.and_then(|f| f.netbox.as_ref()) {
        Some(rec) => rec.dns_name.as_deref().unwrap_or("(reserved, no name)"),
        None => "(not in NetBox)",
    };
    let ptr = facts.and_then(|f| f.ptr.as_deref()).unwrap_or("(no PTR)");
    let live = if facts.is_some_and(|f| f.live) { "yes" } else { "no" };

    let tx = x + 2;
    btxt(buf, tx, y + 1, &row.addr.to_string(), s_head().bg(bgc));
    btxt(buf, tx, y + 2, &format!("status : {}", status_label(row.status)), status_style(row.status).bg(bgc));
    btxt(buf, tx, y + 4, &format!("NetBox : {netbox}"), s_normal().bg(bgc));
    btxt(buf, tx, y + 5, &format!("DNS PTR: {ptr}"), s_normal().bg(bgc));
    btxt(buf, tx, y + 6, &format!("live   : {live}"), s_normal().bg(bgc));
    btxt(buf, tx, y + 7, &format!("why    : {}", explain(row.status)), s_dim().bg(bgc));
}

/// A one-line explanation of what a verdict means, for the inspect panel.
fn explain(s: AddressStatus) -> &'static str {
    match s {
        AddressStatus::Free => "no source claims it — safe to allocate",
        AddressStatus::Allocated => "in NetBox and DNS, and the names agree",
        AddressStatus::NetBoxOnly => "reserved in NetBox, no PTR yet",
        AddressStatus::DnsOnly => "has a PTR but NetBox has no record (drift)",
        AddressStatus::LiveUnregistered => "answers on the wire, in neither NetBox nor DNS",
        AddressStatus::Conflict => "NetBox name and the PTR disagree",
    }
}

/// The one-line status tally: each non-zero bucket in its own colour.
fn count_bar(buf: &mut Buffer, x: u16, y: u16, c: &Counts) {
    let mut cx = x;
    let seg = |buf: &mut Buffer, cx: &mut u16, label: &str, n: usize, style: Style| {
        let text = format!("{label} {n}  ");
        btxt(buf, *cx, y, &text, style);
        *cx += text.len() as u16;
    };
    seg(buf, &mut cx, "free", c.free, s_ok());
    seg(buf, &mut cx, "dns-only", c.dns_only, s_warn());
    seg(buf, &mut cx, "netbox-only", c.netbox_only, s_accent());
    seg(buf, &mut cx, "live", c.live_unregistered, s_err());
    seg(buf, &mut cx, "conflict", c.conflict, status_style(AddressStatus::Conflict));
    seg(buf, &mut cx, "allocated", c.allocated, s_dim());
}
