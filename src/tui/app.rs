// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! TUI orchestrator: application state, the event loop, key routing, and render
//! dispatch. One screen for now — the reconciled address table.

use std::io;
use std::net::Ipv4Addr;
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{Event, KeyEvent};
use mullion::{backend::CrosstermBackend, style::Style, EventReader, GraphCanvas, KeyCode, Terminal};

use super::draw;
use super::focus::ListCursor;
use super::theme::{s_dim, s_err, s_warn};
use crate::config::Config;
use crate::graph::DnsGraph;
use crate::live;
use crate::plan::{Allocation, Plan};
use crate::reconcile::{self, AddressFacts, AddressRow, Cidr, Counts};
use crate::sources::Vantage;

/// The result the live-gather thread sends back.
type LiveResult = anyhow::Result<Vec<AddressFacts>>;

/// The result the allocate-apply thread sends back (a log of what it did).
type ApplyResult = anyhow::Result<String>;

/// Which step of the allocate flow the overlay is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocPhase {
    /// Typing the FQDN for the address.
    Naming,
    /// Reviewing the built plan before applying.
    Preview,
}

/// The in-progress "allocate this address" flow.
pub struct AllocFlow {
    /// The address being allocated.
    pub addr: Ipv4Addr,
    /// The FQDN being typed.
    pub input: String,
    /// The plan, once built (Preview phase).
    pub plan: Option<Plan>,
    /// Which step we're on.
    pub phase: AllocPhase,
}

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

    /// Connection settings, used to gather live data on demand.
    pub cfg: Config,
    /// `true` while a background live-gather is running.
    pub loading: bool,
    /// `true` while a background allocate-apply is running.
    pub applying: bool,
    /// A short status line (message, is_error) shown in the header.
    pub status: Option<(String, bool)>,
    /// The in-progress allocate flow, if any.
    pub alloc: Option<AllocFlow>,
    /// Channel to the in-flight live-gather thread, if any.
    live_rx: Option<mpsc::Receiver<LiveResult>>,
    /// Channel to the in-flight allocate-apply thread, if any.
    apply_rx: Option<mpsc::Receiver<ApplyResult>>,

    write_mode: bool,
    dry_run: bool,
    quit: bool,
}

impl App {
    /// Build the app by reconciling `facts` over `range`. `live` records whether the
    /// facts are real (from the sources) or the demo fixture; `cfg` lets the TUI
    /// gather live data on demand.
    #[must_use]
    pub fn new(range: Cidr, facts: Vec<AddressFacts>, write_mode: bool, dry_run: bool, live: bool, cfg: Config) -> Self {
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
            cfg,
            loading: false,
            applying: false,
            status: None,
            alloc: None,
            live_rx: None,
            apply_rx: None,
            write_mode,
            dry_run,
            quit: false,
        }
    }

    /// Kick off a live gather on a background thread (no-op if one is running).
    ///
    /// The SSH sweep takes tens of seconds, so it runs off-thread and reports back
    /// through a channel; the UI keeps redrawing and stays responsive meanwhile.
    fn start_live_gather(&mut self) {
        if self.loading {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cfg = self.cfg.clone();
        let range = self.range;
        std::thread::spawn(move || {
            let _ = tx.send(live::gather_live(&range, &cfg));
        });
        self.live_rx = Some(rx);
        self.loading = true;
        self.status = Some(("gathering live data…".to_string(), false));
    }

    /// Check whether the background gather has finished; apply or report its result.
    /// Called once per loop tick.
    pub fn poll_live(&mut self) {
        let Some(rx) = &self.live_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(facts)) => self.apply_live(facts),
            Ok(Err(e)) => {
                self.status = Some((format!("live load failed: {e}"), true));
                self.loading = false;
                self.live_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = false;
                self.live_rx = None;
            }
        }
    }

    /// Replace the current data with freshly gathered live facts and rebuild views.
    fn apply_live(&mut self, facts: Vec<AddressFacts>) {
        self.rows = reconcile::reconcile(self.range, &facts);
        self.counts = reconcile::counts(&self.rows);
        self.graph = DnsGraph::from_rows(&self.rows);
        self.graph_canvas = self.graph.layout();
        self.facts = facts;
        self.live = true;
        self.loading = false;
        self.live_rx = None;
        self.pan = (0, 0);
        self.cur.clamp(self.rows.len());
        self.status = Some(("live data loaded".to_string(), false));
    }

    /// The raw facts for `addr`, if any source reported it (free addresses have none).
    #[must_use]
    pub fn facts_for(&self, addr: Ipv4Addr) -> Option<&AddressFacts> {
        self.facts.iter().find(|f| f.addr == addr)
    }

    /// Begin allocating the selected row — only free addresses qualify.
    fn start_alloc(&mut self) {
        let Some(row) = self.rows.get(self.cur.cursor) else {
            return;
        };
        if !row.status.is_free() {
            self.status = Some(("only free addresses can be allocated".to_string(), true));
            return;
        }
        self.detail = false;
        self.alloc = Some(AllocFlow { addr: row.addr, input: String::new(), plan: None, phase: AllocPhase::Naming });
    }

    /// Keys while the allocate overlay is open.
    fn on_key_alloc(&mut self, code: KeyCode) {
        let phase = match &self.alloc {
            Some(f) => f.phase,
            None => return,
        };
        match phase {
            AllocPhase::Naming => match code {
                KeyCode::Esc => self.alloc = None,
                KeyCode::Enter => self.build_alloc_plan(),
                KeyCode::Backspace => {
                    if let Some(f) = &mut self.alloc {
                        f.input.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(f) = &mut self.alloc {
                        f.input.push(c);
                    }
                }
                _ => {}
            },
            AllocPhase::Preview => match code {
                KeyCode::Esc => self.alloc = None,
                KeyCode::Char('y') | KeyCode::Enter => self.start_apply(),
                _ => {}
            },
        }
    }

    /// Build the allocation plan from the typed name and move to the Preview phase.
    fn build_alloc_plan(&mut self) {
        let (addr, fqdn) = match &self.alloc {
            Some(f) => (f.addr, f.input.trim().to_string()),
            None => return,
        };
        if fqdn.is_empty() {
            self.status = Some(("type a name first".to_string(), true));
            return;
        }
        let alloc = Allocation { addr, prefix_len: self.range.prefix_len, fqdn };
        match Plan::for_allocation(alloc, &self.cfg.netbox_url, Some(&self.rows)) {
            Ok(plan) => {
                if let Some(f) = &mut self.alloc {
                    f.plan = Some(plan);
                    f.phase = AllocPhase::Preview;
                }
            }
            Err(e) => self.status = Some((format!("{e}"), true)),
        }
    }

    /// Whether the TUI may actually push changes: write mode on, dry-run off.
    #[must_use]
    pub fn can_apply(&self) -> bool {
        self.write_mode && !self.dry_run
    }

    /// Apply the previewed plan on a background thread. Refuses unless writes are
    /// enabled, so a read-only or dry-run session can preview but never mutate.
    fn start_apply(&mut self) {
        if !self.can_apply() {
            self.status = Some(("read-only — restart with --write to apply".to_string(), true));
            return;
        }
        let plan = match self.alloc.as_ref().and_then(|f| f.plan.clone()) {
            Some(p) => p,
            None => return,
        };
        let (tx, rx) = mpsc::channel();
        let vantage = self.cfg.vantage.clone();
        let token_pass = self.cfg.token_pass.clone();
        std::thread::spawn(move || {
            let res = live::get_token(&token_pass).and_then(|tok| plan.apply(&Vantage::new(&vantage), &tok));
            let _ = tx.send(res);
        });
        self.apply_rx = Some(rx);
        self.applying = true;
        self.status = Some(("applying…".to_string(), false));
    }

    /// Poll the allocate-apply thread; on completion report and close the flow.
    /// Called once per loop tick.
    pub fn poll_apply(&mut self) {
        let Some(rx) = &self.apply_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(_log)) => {
                self.applying = false;
                self.apply_rx = None;
                self.alloc = None;
                self.status = Some(("allocation applied".to_string(), false));
            }
            Ok(Err(e)) => {
                self.applying = false;
                self.apply_rx = None;
                self.status = Some((format!("apply failed: {e}"), true));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.applying = false;
                self.apply_rx = None;
            }
        }
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

    /// Route one key press, first handling the global keys (view toggle, live load).
    pub fn on_key(&mut self, code: KeyCode) {
        // The allocate overlay captures all keys while open.
        if self.alloc.is_some() {
            self.on_key_alloc(code);
            return;
        }
        match code {
            KeyCode::Tab => {
                self.view = match self.view {
                    View::Table => View::Graph,
                    View::Graph => View::Table,
                };
                return;
            }
            KeyCode::Char('L') => {
                self.start_live_gather();
                return;
            }
            _ => {}
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
            KeyCode::Char('a') => self.start_alloc(),
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
pub fn run(range: Cidr, facts: Vec<AddressFacts>, write_mode: bool, dry_run: bool, live: bool, cfg: Config) -> anyhow::Result<()> {
    let mut app = App::new(range, facts, write_mode, dry_run, live, cfg);
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
        app.poll_live();
        app.poll_apply();
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
        let app = App::new(range, facts, false, false, false, Config::default());
        assert!(app.counts.dns_only >= 10); // the -ipmi/-bmc/iprotect drift
        assert_eq!(app.counts.live_unregistered, 1); // the .90 squatter
        assert_eq!(app.counts.netbox_only, 5);
        assert!(app.counts.free > 200);
    }

    #[test]
    fn renders_without_panicking_at_many_sizes() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, Config::default());
        for (w, h) in [(120u16, 50u16), (80, 24), (40, 10), (24, 6), (20, 4)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            draw::screen(&mut buf, &mut app);
        }
    }

    #[test]
    fn graph_view_renders_and_pans_without_panicking() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, Config::default());
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
        let mut app = App::new(range, facts, false, false, false, Config::default());
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
    fn applying_live_facts_switches_source_and_rebuilds() {
        let (range, demo) = fixture::demo();
        let mut app = App::new(range, demo, false, false, false, Config::default());
        assert!(!app.live);
        app.apply_live(vec![AddressFacts {
            addr: "10.87.3.5".parse().unwrap(),
            netbox: None,
            ptr: Some("thing.nfra.nl.".into()),
            live: false,
        }]);
        assert!(app.live && !app.loading);
        assert_eq!(app.counts.dns_only, 1); // the one supplied PTR
        assert!(app.status.as_ref().is_some_and(|(m, e)| m.contains("loaded") && !*e));
    }

    #[test]
    fn allocate_flow_builds_plan_and_gates_apply() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, Config::default());
        // Cursor 0 is 10.87.3.1 (free in the fixture).
        app.on_key(KeyCode::Char('a'));
        assert!(app.alloc.is_some());
        for c in "dop370-ipmi.nfra.nl".chars() {
            app.on_key(KeyCode::Char(c));
        }
        app.on_key(KeyCode::Enter); // build the plan → Preview
        let flow = app.alloc.as_ref().unwrap();
        assert_eq!(flow.phase, AllocPhase::Preview);
        assert!(flow.plan.is_some());
        // Read-only: 'y' must NOT apply; it errors and keeps the overlay open.
        app.on_key(KeyCode::Char('y'));
        assert!(app.alloc.is_some());
        assert!(app.status.as_ref().is_some_and(|(_, e)| *e));
        app.on_key(KeyCode::Esc);
        assert!(app.alloc.is_none());
    }

    #[test]
    fn allocate_refuses_a_taken_row() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, Config::default());
        app.cur.cursor = 10; // 10.87.3.11, a dns-only (taken) row
        app.on_key(KeyCode::Char('a'));
        assert!(app.alloc.is_none());
        assert!(app.status.as_ref().is_some_and(|(_, e)| *e));
    }


    #[test]
    fn next_free_lands_on_a_free_address() {
        let (range, facts) = fixture::demo();
        let mut app = App::new(range, facts, false, false, false, Config::default());
        app.jump_next_free();
        assert!(app.rows[app.cur.cursor].status.is_free());
    }
}
