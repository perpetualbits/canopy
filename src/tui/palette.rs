// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Swappable colour **schemes** for the IP map, with runtime **knobs**.
//!
//! The map draws the Hilbert curve over cells whose colour encodes two things: how full a
//! block is (occupancy) and where the cell sits along the curve (position). A [`Scheme`] is
//! one algorithm for turning `(occupancy, position)` into a `(background, curve)` colour
//! pair; you cycle schemes live and tune a shared vector of [`Knobs`] on the fly. Keeping the
//! parameters a flat, bounded vector (see [`KNOBS`]) means a scheme can later be searched or
//! bred/evolved, not just hand-tuned.
//!
//! Design idea the schemes lean on: give the **background a chroma-biased** signal (hue and
//! saturation carry occupancy) and the **curve a luma-biased** one (brightness contrast
//! carries the path), so the two never fight — you read fullness as colour and the path as a
//! bright line at once.

use mullion::style::Color;

/// Linear interpolation.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// HSL → RGB. `h` in degrees (wrapped), `s`/`l` in `[0, 1]`. The standard conversion —
/// working in HSL lets a scheme steer hue, saturation and lightness independently.
#[must_use]
pub fn hsl_rgb(h: f32, s: f32, l: f32) -> Color {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let l = l.clamp(0.0, 1.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to(r), to(g), to(b))
}

/// Normalized Rec. 601 luma of a colour, `[0, 1]`.
fn luma(c: Color) -> f32 {
    let Color::Rgb(r, g, b) = c else { return 0.5 };
    (0.30 * f32::from(r) + 0.59 * f32::from(g) + 0.11 * f32::from(b)) / 255.0
}

/// Map an occupancy fraction `f ∈ [0, 1]` onto `[0, 1]` on a logarithmic scale of `decades`
/// (so a barely-used block is not indistinguishable from an empty one).
fn occ(f: f32, decades: f32) -> f32 {
    if f <= 0.0 {
        0.0
    } else {
        (1.0 + f.log10() / decades.max(0.5)).clamp(0.0, 1.0)
    }
}

/// A luma-biased curve colour: a line that contrasts its background `bg` (dark on a bright
/// cell, bright on a dark one) but is tinted by the curve position `pos` — so the path is
/// always legible yet its hue drifts along the curve. `sat` sets how strong the tint is.
fn curve_fg(bg: Color, pos: f32, sat: f32) -> Color {
    let base = if luma(bg) > 0.5 { 0.18 } else { 0.9 };
    hsl_rgb(pos * 300.0, sat, base)
}

/// The tunable parameters shared by every scheme — a flat, bounded vector (see [`KNOBS`]),
/// so it can be hand-tuned now and searched/evolved later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knobs {
    /// Decades the occupancy log scale spans.
    pub decades: f32,
    /// Maximum background lightness — the knob that tames an "in-your-face" white top end.
    pub ceiling: f32,
    /// Minimum background lightness (a barely-used block).
    pub floor: f32,
    /// Background saturation (chroma-biased signal).
    pub bg_sat: f32,
    /// Curve-line saturation (the luma-biased signal's tint).
    pub fg_sat: f32,
    /// Occupancy hue at the empty end (degrees).
    pub hue_lo: f32,
    /// Occupancy hue at the full end (degrees).
    pub hue_hi: f32,
}

impl Default for Knobs {
    fn default() -> Self {
        // Deliberately no white/yellow blaze at the top: lightness capped at 0.60, and the
        // full end is a warm orange (hue 40), not amber — occupancy still reads, but calmly.
        Knobs { decades: 3.0, ceiling: 0.60, floor: 0.12, bg_sat: 0.85, fg_sat: 0.35, hue_lo: 0.0, hue_hi: 40.0 }
    }
}

/// The knob table: `(name, min, max, step)`, indexed by the active-knob selector. Editing
/// this is all it takes to expose a new parameter to the UI and to any future search.
pub const KNOBS: [(&str, f32, f32, f32); 7] = [
    ("decades", 1.0, 6.0, 0.5),
    ("ceiling", 0.35, 1.0, 0.04),
    ("floor", 0.0, 0.5, 0.03),
    ("bg_sat", 0.0, 1.0, 0.05),
    ("fg_sat", 0.0, 1.0, 0.05),
    ("hue_lo", 0.0, 360.0, 12.0),
    ("hue_hi", 0.0, 360.0, 12.0),
];

impl Knobs {
    /// Read knob `i` (see [`KNOBS`]).
    #[must_use]
    pub fn get(&self, i: usize) -> f32 {
        [self.decades, self.ceiling, self.floor, self.bg_sat, self.fg_sat, self.hue_lo, self.hue_hi]
            .get(i)
            .copied()
            .unwrap_or(0.0)
    }

    /// Write knob `i`.
    fn set(&mut self, i: usize, v: f32) {
        match i {
            0 => self.decades = v,
            1 => self.ceiling = v,
            2 => self.floor = v,
            3 => self.bg_sat = v,
            4 => self.fg_sat = v,
            5 => self.hue_lo = v,
            6 => self.hue_hi = v,
            _ => {}
        }
    }

    /// Nudge knob `i` by `dir` (±1) steps, clamped to its range.
    pub fn adjust(&mut self, i: usize, dir: f32) {
        if let Some(&(_, lo, hi, step)) = KNOBS.get(i) {
            self.set(i, (self.get(i) + dir * step).clamp(lo, hi));
        }
    }
}

/// A colour scheme — one algorithm mapping `(occupancy, curve position)` to a
/// `(background, curve)` colour pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scheme {
    /// Chroma-biased background (hue = occupancy, capped lightness — no blinding white),
    /// luma-biased curve tinted by position. The default.
    #[default]
    ChromaLuma,
    /// Warm heat background (red→amber by occupancy), a plain luma-contrast curve.
    Heat,
    /// A dim background; the **curve** carries a full-spectrum hue by position, so the path's
    /// direction pops.
    CurveHue,
    /// Monochrome — grey background by occupancy, contrast-grey curve (low-colour terminals).
    Mono,
}

impl Scheme {
    /// Every scheme, in cycle order.
    pub const ALL: [Scheme; 4] = [Scheme::ChromaLuma, Scheme::Heat, Scheme::CurveHue, Scheme::Mono];

    /// The next scheme in the cycle.
    #[must_use]
    pub fn cycle(self) -> Scheme {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// A short display name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Scheme::ChromaLuma => "chroma/luma",
            Scheme::Heat => "heat",
            Scheme::CurveHue => "curve-hue",
            Scheme::Mono => "mono",
        }
    }

    /// The `(background, curve)` colours for a cell with occupancy `frac ∈ [0, 1]` at curve
    /// position `pos ∈ [0, 1]` (its fraction along the Hilbert path), given the `k`nobs.
    #[must_use]
    pub fn paint(self, frac: f32, pos: f32, k: &Knobs) -> (Color, Color) {
        let t = occ(frac, k.decades);
        let empty = frac <= 0.0;
        match self {
            Scheme::ChromaLuma => {
                let light = if empty { 0.06 } else { lerp(k.floor, k.ceiling, t) };
                let bg = hsl_rgb(lerp(k.hue_lo, k.hue_hi, t), k.bg_sat, light);
                (bg, curve_fg(bg, pos, k.fg_sat))
            }
            Scheme::Heat => {
                let light = if empty { 0.06 } else { lerp(k.floor, k.ceiling, t) };
                let bg = hsl_rgb(lerp(0.0, 55.0, t), k.bg_sat, light);
                let fg = if luma(bg) > 0.5 { Color::Rgb(20, 20, 20) } else { Color::Rgb(225, 225, 225) };
                (bg, fg)
            }
            Scheme::CurveHue => {
                let light = if empty { 0.05 } else { lerp(0.08, 0.34, t) };
                let bg = hsl_rgb(lerp(k.hue_lo, k.hue_hi, t), k.bg_sat * 0.5, light);
                (bg, hsl_rgb(pos * 330.0, k.fg_sat.max(0.6), 0.62))
            }
            Scheme::Mono => {
                let light = if empty { 0.06 } else { lerp(0.15, k.ceiling, t) };
                let g = (light * 255.0).round() as u8;
                let fg = if light > 0.5 { Color::Rgb(20, 20, 20) } else { Color::Rgb(230, 230, 230) };
                (Color::Rgb(g, g, g), fg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_endpoints() {
        assert_eq!(hsl_rgb(0.0, 1.0, 0.5), Color::Rgb(255, 0, 0)); // pure red
        assert_eq!(hsl_rgb(120.0, 1.0, 0.5), Color::Rgb(0, 255, 0)); // pure green
        assert_eq!(hsl_rgb(0.0, 0.0, 1.0), Color::Rgb(255, 255, 255)); // white
        assert_eq!(hsl_rgb(0.0, 0.0, 0.0), Color::Rgb(0, 0, 0)); // black
    }

    #[test]
    fn ceiling_tames_the_top_end() {
        // A full block under the default ceiling is NOT blinding white.
        let (bg, _) = Scheme::ChromaLuma.paint(1.0, 0.5, &Knobs::default());
        assert!(luma(bg) < 0.85, "full-block bg too bright: luma={}", luma(bg));
        // Raising the ceiling knob brightens it.
        let k = Knobs { ceiling: 1.0, ..Knobs::default() };
        let (bright, _) = Scheme::ChromaLuma.paint(1.0, 0.5, &k);
        assert!(luma(bright) > luma(bg));
    }

    #[test]
    fn schemes_cycle_and_paint_valid_colours() {
        let mut s = Scheme::default();
        for _ in 0..Scheme::ALL.len() {
            for &(f, p) in &[(0.0, 0.0), (0.001, 0.5), (1.0, 1.0)] {
                let (bg, fg) = s.paint(f, p, &Knobs::default());
                assert!(matches!(bg, Color::Rgb(..)) && matches!(fg, Color::Rgb(..)));
            }
            s = s.cycle();
        }
        assert_eq!(s, Scheme::default(), "cycle returns to the start");
    }

    #[test]
    fn knob_adjust_clamps_to_range() {
        let mut k = Knobs::default();
        for _ in 0..100 {
            k.adjust(1, 1.0); // ceiling up, many times
        }
        assert_eq!(k.get(1), KNOBS[1].2); // pinned at max, not overshot
        for _ in 0..100 {
            k.adjust(1, -1.0);
        }
        assert_eq!(k.get(1), KNOBS[1].1); // pinned at min
    }
}
