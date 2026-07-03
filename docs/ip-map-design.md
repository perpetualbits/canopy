<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026  Epsilon Null Operation -->

# IP map — layout & legend design

**Decided: Hilbert curve, with cell-cursor zoom (shipped).** The map lays the range on
a Hilbert curve as a `2^order × 2^order` grid of shaded blocks (`·` free, `░▒▓█` used),
aggregated from the bounded facts in `O(facts)`. A highlighted cursor moves over the
grid (`hjkl`/arrows); `Enter` zooms into the cell under it — always a clean
`/(prefix + 2·order)` subnet — and `Backspace`/`-` zooms back out. A few steps take a
`/8` down to a `/24` the table and tree resolve to individual addresses. The sections
below record the reasoning that led here.

The first cut rendered the range as a raster `cols×rows` grid — it worked, but the
**layout** was the real design question, because it decides both what the legend/axes
can say *and* how cleanly "zoom into a region" lands on a subnet ("in a few steps you
arrive at subnets you CAN resolve").

## The tension

- The first cut is **raster** (row-major): address increases left→right, then wraps.
  Clean linear counting, but a screen **rectangle ≠ a subnet** (rows wrap → jagged,
  non-contiguous range), and x/y don't independently mean anything, so an "x/y legend"
  can only label linear position.
- The zoom goal wants the opposite: a region you box should *be* a resolvable CIDR.

## Options

| Layout | Zoom-box is a subnet? | Legend | Effort |
|--------|-----------------------|--------|--------|
| **Raster (current)** | ✗ (rows wrap) | linear position + address span only | done |
| **Block-aligned raster** | per-cell only (a cell = a `/k`); multi-cell box wraps | "cell = `/k`; row r, col c → block" | S–M |
| **Hilbert curve** | ✓ every square region = a contiguous CIDR | labels the recursive quadrant/block structure | M–L |

### Recommendation: **Hilbert curve**

It's the only layout where a zoomed square *is* a clean subnet, which is exactly the
vision's "zoom → resolvable subnet in a few steps". It's the classic map-of-the-
internet layout: subnets stay contiguous and square at every scale. Cost: the mapping
`index ↔ (x, y)` is a real (but small, well-understood) algorithm, and the legend is
about **block boundaries**, not simple linear ticks.

## If Hilbert — implementation sketch

- **Order.** A Hilbert curve of order `k` fills a `2^k × 2^k` grid = `4^k` cells. Pick
  `k` so `4^k` cells fit the terminal *and* divide the range's host bits evenly; each
  cell = a `/(prefix + 2k)` block (before host-bit rounding).
- **Mapping.** Pure `hilbert_d2xy(order, d) -> (x, y)` / `hilbert_xy2d(order, x, y) ->
  d` (standard bit-twiddle, ~10 lines each, fully unit-testable — bijective, and a
  `+1` step in `d` is always an adjacent cell). Bucket facts by `d = index / block`,
  then place at `d2xy`.
- **Legend.** Since a `2^m × 2^m` sub-square is a contiguous `/(prefix + 2(k−m))`
  block, label the map by its quadrant tree: the whole map is the range's CIDR; on
  hover/selection show the CIDR of the square under the cursor. Optional faint guides
  at major quadrant boundaries (each = a shorter-prefix block). No linear x/y ticks —
  they'd be misleading.
- **Zoom.** Select a cell (or drag a square) → the new range is that block's CIDR →
  rebuild the grid over it. A few steps take `/8 → /12 → /16 → …` down to a `/24`-ish
  block the table/tree already resolve to individual addresses. `host_at`/`host_index`
  already give the address↔index math; zoom just narrows `range`.

## If block-aligned raster (simpler fallback)

Round `cols`/`rows` to powers of two and align the block size to a prefix boundary so
each cell is a real `/k`. Zoom targets a *cell* (clean `/k`), not an arbitrary
rectangle. Legend: "each cell = `/k`", plus the address span. Much less code than
Hilbert; loses only whole-rectangle zoom.

## Open sub-questions

1. ~~Layout: Hilbert vs block-aligned raster vs keep raster.~~ **Decided: Hilbert.**
2. Shade vs colour for density — the current `░▒▓█` in one accent colour is neutral;
   a green→red heat would read "fuller = hotter" (no free space). Which reading do we
   want? **Still open.**
3. ~~Cell aspect.~~ Cells are 2 terminal columns wide (squarer), which keeps the
   Hilbert quadrant structure readable. **Settled.**
