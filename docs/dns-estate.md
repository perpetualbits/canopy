<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026  Epsilon Null Operation -->

# The ASTRON DNS estate (as discovered)

The map netpush's `dns` model encodes — and the topology the future node-graph view
will draw. Recorded 2026-07-03 from live inspection.

## Zones, servers, serial schemes

| Zone | Master server | File | Serial scheme | netpush status |
|------|---------------|------|---------------|----------------|
| `nfra.nl` (forward) | **dns1.astron.nl** | `/etc/bind/master/db.nfra.nl` | `YYYYMMDDnn` (e.g. `2026070300`) | ✅ safe edit proven |
| `astron.nl` (forward) | **dns1.astron.nl** | `/etc/bind/master/db.astron.nl` | `YYYYMMDDnn` | model ready |
| `10.in-addr.arpa` (reverse, all 10.x) | **ntserver1.nfra.nl** | *TBD — confirm on ntserver1* | plain integer (e.g. `3057388`) | 🚧 gated until file path known |
| `*.lofar` + LOFAR reverse | **lcs020.control.lofar** | `/var/lib/named/master/*` | mixed | out of scope (separate estate) |

Secondaries for `10.in-addr.arpa`: ntserver8, ntserver16, ns0.jive.nl, ns1.jive.nl.

## Consequences for the write path

- A host like `dop370-ipmi.nfra.nl` at `10.87.3.69` needs **two edits on two servers**:
  the forward `A` on dns1 and the reverse `PTR` on ntserver1. They have **different
  serial schemes**, so the bump logic is per-zone ([`dns::serial::SerialScheme`]).
- dns1 is **DNSSEC inline-signed**: edit the unsigned `db.nfra.nl`, `rndc reload`, and
  named re-signs. `named-checkzone` on a copy before swap-in catches malformed edits.
- `gen_ptr.py` on dns1 is **IPv6-only** — it does *not* manage the IPv4 reverse. Do
  not use it for `in-addr.arpa`.

## Open items before the reverse can be automated

1. SSH access + zone file path on **ntserver1.nfra.nl** for `10.in-addr.arpa`.
2. Confirm ntserver1 uses BID/`named-checkzone`/`rndc` (so the same safe-edit applies)
   or a different server that needs its own `Zone` recipe.

## Why this is the node-graph seed

Each row above is a **group node** (a zone) pinned to a **server**; records inside are
child nodes; CNAME/NS/PTR are **edges** to other names (`dns::Record::target_name`).
Draw it and you see the estate — which is the whole point of the node-graph milestone.
