<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026  Epsilon Null Operation -->

# netpush — the long vision

Recorded 2026-07-03. This is the north star; the [README roadmap](../README.md)
tracks the near-term steps toward it.

## DNS as a mullion node graph

Work with DNS through **mullion's node graph**. DNS is naturally a good fit:

- It has a **nested structure** (zones, subdomains, records) that maps onto nested
  nodes.
- Real-world groupings — e.g. a **cluster of computers** — are a named grouping, so
  rendering them as a labelled group node gives an instant overview of the estate.

The graph is the interface *and* the model: you see the shape of DNS, not a flat list.

## Layout you can trust

The user must be able to organise the graph **any way they want**:

- **By hand** — place nodes and **hand-route the wires** yourself; or
- **Automatically** — let the engine lay out nodes and route wires.

The automatic layout is the hard, valuable part. Aim for a **wiring algorithm that
eases cognitive complexity** by making node graphs:

- **regular** and **aligned**,
- with **few crossings**,
- **compact**, and
- **flexible**.

Key idea: **forbid certain local minima in wiring-space** so the optimiser is *forced*
toward the layout you actually want, instead of settling into a technically-fine-but-
ugly arrangement. Constrain the search space to shape the result.

## The transition play

1. **Lay out** the *current* DNS nicely as a node graph.
2. **Transform** it — on the graph — into what we want it to become.
3. **Migrate** the backing server to something modern, e.g. **Technitium** or
   **PowerDNS**, and **use netpush to drive the transition** (diff current → target,
   push the changes, verify).

The reconciler built in milestone 1 is the seed of this: it already compares "what is"
across sources. The graph is "what is" and "what we want", side by side.

## Then: roles / personalities, one foundation

Once the migration is done, the **same foundation** grows different faces:

- a **quick DNS editor** (fast, keyboard-driven record edits),
- a **DNS design application** with node graphs (plan estates visually),
- and many more.

All sharing the reconcile core, the source layer, and the mullion UI — different
*personalities* over one engine.

## Live wires: the bitstream

Use mullion's **bitstream feature** to make the wires *carry information*, not just
connect nodes. A wire renders as a stream of little coloured squares — **closed = 1,
open = 0** — so a link visibly shows what flows through it: the route it carries, the
gateway behind it, utilisation, VLAN, whatever we bind to it. The graph stops being a
static diagram and becomes a live view of the network.

## Beyond DNS: switches and routers

Extend the same model to the **switching/routing fabric**:

- Reach **switch and router configs** through the tool.
- **Laying out a line by hand changes the config on the switch** — draw the topology
  you want and the tool pushes it (VLAN assignment, port config, routes) — *only when
  you have explicitly put it in "apply" mode*. Design-only by default.

At that point netpush is a **real network tool**: one node graph over DNS, IPAM, and
the L2/L3 fabric, where the picture and the running config are the same thing.

## AAA & security (non-negotiable for the above)

Pushing config to switches/routers raises the stakes hard. Before the fabric-write
features land we need proper **AAA** (authentication, authorization, accounting) and
security: who may change what, every change attributed and logged, least-privilege
credentials, explicit apply-mode gating, and a full audit trail. The read-only-by-
default, `--dry-run`, diff-before-apply discipline from the DNS side is the seed —
the fabric side must be stricter still.

## Why this order

We earn the graph by first getting the boring plumbing right (live sources → reconcile
→ push). The plumbing is milestone 2–3. The graph is what makes it a *design* tool
rather than a CRUD tool — but it needs trustworthy data underneath, which is exactly
what the reconciler guarantees. The bitstream wires, the switch/router fabric, and the
AAA that must gate them all come *after* the graph and the DNS write path are solid.
