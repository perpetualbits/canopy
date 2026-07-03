// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! TUI orchestrator: application state, the event loop, key routing, and render
//! dispatch. One screen for now — the reconciled address table.

use std::io;
use std::net::Ipv4Addr;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};
use mullion::{backend::CrosstermBackend, style::Style, EventReader, GraphCanvas, KeyCode, Terminal};

use super::draw;
use super::focus::ListCursor;
use super::theme::{s_dim, s_err, s_warn};
use crate::graph::DnsGraph;
use crate::reconcile::{self, AddressFacts, AddressRow, Cidr, Counts};

/// Idle redraw cap (~20 fps) so the UI stays responsive without busy-looping.
const RENDER_TICK: Duration = Duration::from_millis(50);

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The reconciled address table.
    Table,
    /// The cluster node graph.
    Graph,
}

/// The whole application state.
pub struct App {
    /// The range being browsed.
    pub range: Cidr,
    /// The reconciled rows, one per host address, in address order.
    pub rows: Vec<AddressRow>,
    /// The raw per-address facts behind the rows — kept so the inspect panel can
    /// show *why* an address is classified the way it is.
    pub facts: Vec<AddressFacts>,
    /// Cached status tally for the header.
    pub counts: Counts,
    /// Whether `facts` came from the live sources (`true`) or the demo fixture.
    pub live: bool,
    /// The list cursor (selection + scroll offset).
    pub cur: ListCursor,
    /// Body height measured at the last render — used for PageUp/PageDown.
    pub page: usize,
    /// Whether the inspect panel for the selected row is open.
    pub detail: bool,

    /// Which screen is showing.
    pub view: View,
    /// The cluster graph built from `rows`.
    pub graph: DnsGraph,
    /// The laid-out canvas for the graph view.
    pub graph_canvas: GraphCanvas,
    /// Pan offset (canvas cells) for the graph view.
    pub pan: (u16, u16),

    write_mode: bool,
    dry_run: bool,
    quit: bool,
}

impl App {
    /// Build the app by reconciling `facts` over `range`. `live` records whether the
    /// facts are real (from the sources) or the demo fixture.
    #[must_use]
    pub fn new(range: Cidr, facts: Vec<AddressFacts>, write_mode: bool, dry_run: bool, live: bool) -> Self {
        let rows = reconcile::reconcile(range, &facts);
        let counts = reconcile::counts(&rows);
        let graph = DnsGraph::from_rows(&rows);
        let graph_canvas = graph.layout();
        App {
            range,
            rows,
            facts,
            counts,
            live,
            cur: ListCursor::new(),
            page: 10,
            detail: false,
            view: View::Table,
            graph,
            graph_canvas,
            pan: (0, 0),
            write_mode,
            dry_run,
            quit: false,
        }
    }

    /// The raw facts for `addr`, if any source reported it (free addresses have none).
    #[must_use]
    pub fn facts_for(&self, addr: Ipv4Addr) -> Option<&AddressFacts> {
        self.facts.iter().find(|f| f.addr == addr)
    }

    /// The mode badge shown top-right: colourful because write mode is dangerous.
    #[must_use]
    pub fn mode_label(&self) -> (&'static str, Style) {
        if self.dry_run {
            ("DRY-RUN", s_warn())
        } else if self.write_mode {
            ("WRITE", s_err())
        } else {
            ("READ-ONLY", s_dim())
        }
    }

    /// Route one key press, first handling the global view toggle.
    pub fn on_key(&mut self, code: KeyCode) {
        if code == KeyCode::Tab {
            self.view = match self.view {
                View::Table => View::Graph,
                View::Graph => View::Table,
            };
            return;
        }
        match self.view {
            View::Table => self.on_key_table(code),
            View::Graph => self.on_key_graph(code),
        }
    }

    /// Keys for the table view: list navigation and the inspect panel.
    fn on_key_table(&mut self, code: KeyCode) {
        let len = self.rows.len();
        match code {
            KeyCode::Char('q') => self.quit = true,
            // Esc closes the inspect panel if open, otherwise quits.
            KeyCode::Esc => {
                if self.detail {
                    self.detail = false;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Enter => self.detail = !self.detail,
            KeyCode::Char('j') | KeyCode::Down => self.cur.down(len),
            KeyCode::Char('k') | KeyCode::Up => self.cur.up(),
            KeyCode::Char('g') | KeyCode::Home => self.cur.reset(),
            KeyCode::Char('G') | KeyCode::End => self.cur.end(len),
            KeyCode::PageUp => self.cur.page(-(self.page as isize), len),
            KeyCode::PageDown => self.cur.page(self.page as isize, len),
            KeyCode::Char('f') => self.jump_next_free(),
            _ => {}
        }
    }

    /// Keys for the graph view: pan the window across the canvas.
    fn on_key_graph(&mut self, code: KeyCode) {
        let (cw, ch) = self.graph_canvas.size();
        const STEP: u16 = 4;
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Left | KeyCode::Char('h') => self.pan.0 = self.pan.0.saturating_sub(STEP),
            KeyCode::Right | KeyCode::Char('l') => self.pan.0 = (self.pan.0 + STEP).min(cw.saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => self.pan.1 = self.pan.1.saturating_sub(STEP),
            KeyCode::Down | KeyCode::Char('j') => self.pan.1 = (self.pan.1 + STEP).min(ch.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.pan = (0, 0),
            _ => {}
        }
    }

    /// Move the cursor to the next free address after the current one, wrapping
    /// around the list. Does nothing if there are no free addresses.
    fn jump_next_free(&mut self) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        for step in 1..=len {
            let i = (self.cur.cursor + step) % len;
            if self.rows[i].status.is_free() {
                self.cur.cursor = i;
                return;
            }
        }
    }
}

/// Enter the alternate screen, run the loop, and always restore the terminal.
///
/// # Errors
/// Propagates terminal setup / draw errors.
pub fn run(range: Cidr, facts: Vec<AddressFacts>, write_mode: bool, dry_run: bool, live: bool) -> anyhow::Result<()> {
    let mut app = App::new(range, facts, write_mode, dry_run, live);
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    term.enter()?;
    let result = main_loop(&mut term, &mut app);
    term.leave()?;
    result
}

/// The draw / read-key loop: redraw, then wait up to one tick for a key.
fn main_loop(term: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> anyhow::Result<()> {
    let reader = EventReader::new();
    while !app.quit {
        term.draw(|buf| match app.view {
            View::Table => draw::screen(buf, app),
            View::Graph => super::graph::screen(buf, app),
        })?;
        if let Some(Event::Key(KeyEvent { code, .. })) = reader.recv_timeout(RENDER_TICK) {
            app.on_key(code);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use mullion::{Buffer, Rect};

    #[test]
    fn fixture_reconciles_to_expected_statuses() {
        let (range, facts) = fixture::demo();
        let app = App::new(range, facts, false, false, false);
        assert!(app.counts.dns_only >= 10); // the -ipmi/-bmc/iprotect drift
        assert_eq!(app.counts.live_unregistered, 1); // the .90 squatter
        assert_eq!(app.counts.netbox_only, 5);
        assert!(app.counts.free > 200);
    }

    #[test]
    fn renders_without_panicking_at_many_sizes() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false);
        for (w, h) in [(120u16, 50u16), (80, 24), (40, 10), (24, 6), (20, 4)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            draw::screen(&mut buf, &mut app);
        }
    }

    #[test]
    fn graph_view_renders_and_pans_without_panicking() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false);
        app.on_key(KeyCode::Tab); // switch to the graph view
        assert_eq!(app.view, View::Graph);
        assert!(app.graph.cluster_count() > 0);
        for (w, h) in [(120u16, 50u16), (80, 24), (40, 10), (24, 6)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            crate::tui::graph::screen(&mut buf, &mut app);
            app.on_key(KeyCode::Right); // pan around while rendering
            app.on_key(KeyCode::Down);
        }
        app.on_key(KeyCode::Tab); // back to the table
        assert_eq!(app.view, View::Table);
    }

    #[test]
    fn inspect_panel_toggles_and_renders() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false);
        assert!(!app.detail);
        app.on_key(KeyCode::Enter);
        assert!(app.detail); // Enter opens the inspect panel
        app.cur.cursor = 10; // a dns-only row (10.87.3.11)
        let mut buf = Buffer::empty(Rect::new(0, 0, 90, 22));
        draw::screen(&mut buf, &mut app);
        app.on_key(KeyCode::Esc);
        assert!(!app.detail && !app.quit); // Esc closes the panel, does not quit
    }

    #[test]
    fn next_free_lands_on_a_free_address() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false);
        app.jump_next_free();
        assert!(app.rows[app.cur.cursor].status.is_free());
    }
}
