// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! SOA serials and how to advance them safely. Getting this wrong silently breaks
//! zone transfers to secondaries and DNSSEC re-signing, so the bump is a pure,
//! well-tested function — the I/O (reading the current serial, learning today's
//! date) happens elsewhere and feeds this.
//!
//! Two schemes exist on the ASTRON estate:
//! - **`DateCounter`** — `YYYYMMDDnn`, used by dns1 for `nfra.nl` / `astron.nl`.
//! - **`Counter`** — a plain incrementing integer, used by ntserver1 for the
//!   `10.in-addr.arpa` reverse zone.

/// How a zone's SOA serial advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialScheme {
    /// `YYYYMMDDnn`: today's date times 100, plus a same-day counter.
    DateCounter,
    /// A plain monotonically increasing integer.
    Counter,
}

impl SerialScheme {
    /// The next serial after `current`, given `today` as a `YYYYMMDD` integer.
    ///
    /// How: for `DateCounter` the natural next value is `today·100` (counter `00`),
    /// but if the current serial is already ≥ that — several edits in one day, or a
    /// file dated ahead of the vantage's clock — we simply increment, so the serial
    /// never goes backwards. For `Counter` we just add one. Serials are compared as
    /// plain `u64` here; callers keep them inside the 32-bit DNS range.
    ///
    /// Units: `current` and the result are raw serial numbers; `today` is `YYYYMMDD`.
    #[must_use]
    pub fn next(self, current: u64, today: u32) -> u64 {
        match self {
            SerialScheme::DateCounter => {
                let candidate = u64::from(today) * 100;
                if current >= candidate {
                    current + 1
                } else {
                    candidate
                }
            }
            SerialScheme::Counter => current + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datecounter_rolls_to_today() {
        // dns1's real nfra.nl serial, bumped on 2026-07-03.
        assert_eq!(SerialScheme::DateCounter.next(2_026_010_900, 20_260_703), 2_026_070_300);
    }

    #[test]
    fn datecounter_same_day_increments() {
        // Second edit on 2026-07-03 must go past 2026070300, not reset to it.
        assert_eq!(SerialScheme::DateCounter.next(2_026_070_300, 20_260_703), 2_026_070_301);
        assert_eq!(SerialScheme::DateCounter.next(2_026_070_305, 20_260_703), 2_026_070_306);
    }

    #[test]
    fn datecounter_never_goes_backwards_if_file_ahead() {
        // File dated in the future relative to `today` → still monotonic.
        assert_eq!(SerialScheme::DateCounter.next(2_026_080_100, 20_260_703), 2_026_080_101);
    }

    #[test]
    fn counter_just_increments() {
        // ntserver1's reverse-zone style.
        assert_eq!(SerialScheme::Counter.next(3_057_388, 20_260_703), 3_057_389);
    }
}
