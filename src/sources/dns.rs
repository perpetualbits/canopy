// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! DNS as a fact source: the PTR records actually served. This is the most reliable
//! "is it allocated?" signal we found — it caught addresses NetBox never recorded.
//! We reverse-resolve every host on the vantage (its resolver knows the internal
//! zones), in parallel with bounded fan-out, and collect the answers.

use std::net::IpAddr;

use super::{FactSource, Vantage};
use crate::reconcile::{AddressFacts, Cidr};

/// Reverse-resolves every host in a range via the vantage's resolver.
#[derive(Debug, Clone)]
pub struct DnsSource {
    /// A host whose resolver can see the internal reverse zones.
    pub vantage: Vantage,
    /// Max concurrent lookups (the `xargs -P` fan-out) — bounds the burst on the
    /// resolver and the authoritative reverse server behind it.
    pub concurrency: usize,
    /// Authoritative server to try a **zone transfer** (AXFR) from. When non-empty and
    /// transfer is permitted, one AXFR per `/24` replaces hundreds of `host` lookups;
    /// otherwise we fall back to the per-address sweep. Empty disables AXFR.
    pub axfr_server: String,
}

/// Safety cap on how many `/24` reverse zones an AXFR sweep will transfer. A range that
/// needs more is left to the per-address sweep rather than firing hundreds of transfers.
const MAX_ZONES: usize = 512;

impl FactSource for DnsSource {
    fn gather(&self, range: &Cidr) -> anyhow::Result<Vec<AddressFacts>> {
        self.gather_with_progress(range, |_frac, _label| {})
    }
}

impl DnsSource {
    /// Reverse-resolve every host in `range`, reporting progress through
    /// `on_progress(fraction, label)` as it goes, and return the PTR facts found.
    ///
    /// If an AXFR server is configured and transfer is permitted, this pulls whole `/24`
    /// reverse zones (one query each — dramatically fewer, and far lighter on the DNS
    /// server); otherwise it falls back to the per-address sweep.
    ///
    /// # Errors
    /// Propagates SSH failures.
    pub fn gather_with_progress(
        &self,
        range: &Cidr,
        mut on_progress: impl FnMut(f32, &str),
    ) -> anyhow::Result<Vec<AddressFacts>> {
        // A per-address sweep needs to enumerate the range; a huge (IPv6) range cannot be
        // enumerated. IPv6 reverse-by-AXFR (`ip6.arpa`) is a follow-up, so for now such a
        // range gets no reverse DNS here and relies on NetBox alone.
        if !range.is_enumerable() {
            return Ok(Vec::new());
        }
        // AXFR is the light path (IPv4 `in-addr.arpa` only for now) — but only if the
        // server actually lets us transfer; if not, quietly fall back to the sweep.
        if !self.axfr_server.is_empty() && !range.is_v6() {
            if let Some(facts) = self.try_axfr(range, &mut on_progress)? {
                return Ok(facts);
            }
        }
        self.sweep(range, on_progress)
    }

    /// The per-address reverse sweep: one `host` lookup per address, in parallel with
    /// bounded fan-out.
    ///
    /// A serial `for` loop did one blocking `host` per address — for a /20 that is ~4000
    /// lookups back-to-back, each waiting out a timeout when there is no PTR, so it took
    /// minutes. `xargs -P` runs up to `concurrency` at once (bounding load on the
    /// resolver) and `host -W1` caps each lookup at ~1 s. Each worker prints `T` when
    /// done (a progress tick, streamed back and counted) and `R <ip> <name>` when a PTR
    /// exists; both lines are short enough to be written atomically to the pipe. `$0`
    /// inside the `sh -c` body is the address xargs handed it.
    fn sweep(&self, range: &Cidr, mut on_progress: impl FnMut(f32, &str)) -> anyhow::Result<Vec<AddressFacts>> {
        let ips = host_list(range);
        let par = self.concurrency.max(1);
        let remote = format!(
            "printf '%s\\n' {ips} | xargs -P{par} -n1 sh -c 'h=$(host -W1 \"$0\" 2>/dev/null | sed -n \"s/.*pointer //p\"); printf \"T\\n\"; [ -n \"$h\" ] && printf \"R %s %s\\n\" \"$0\" \"$h\"'"
        );
        let total = range.host_count().max(1);
        let step = (total / 100).max(1); // update ~every 1 % rather than per address
        let mut done = 0u128;
        let mut results = String::new();
        self.vantage.run_streaming(&remote, |line| {
            if line == "T" {
                done += 1;
                if done % step == 0 || done == total {
                    on_progress(done as f32 / total as f32, &format!("DNS reverse sweep {done}/{total}"));
                }
            } else if let Some(rest) = line.strip_prefix("R ") {
                results.push_str(rest);
                results.push('\n');
            }
        })?;
        Ok(parse_ptrs(&results))
    }

    /// Try to pull the reverse PTRs by zone transfer. Returns `Some(facts)` when AXFR is
    /// permitted and done, or `None` when the server refuses (or the range needs more
    /// than [`MAX_ZONES`] zones) so the caller falls back to the sweep.
    ///
    /// Gate: transfer the first zone; if the server refuses we get an empty answer (no
    /// SOA), which we read as "not allowed". Otherwise transfer the rest with bounded
    /// parallelism (transfers are heavier than lookups, so a smaller fan-out), ticking
    /// once per zone. Each answer line is prefixed `R ` so it is told apart from the `T`
    /// zone tick on the shared stream.
    fn try_axfr(&self, range: &Cidr, mut on_progress: impl FnMut(f32, &str)) -> anyhow::Result<Option<Vec<AddressFacts>>> {
        let zones = reverse_zones(range);
        if zones.is_empty() {
            return Ok(None); // too large for AXFR — use the sweep
        }
        let n = zones.len();
        // Probe the first zone. Any error (server unreachable, dig missing) → fall back.
        let probe = match self.axfr_zone(&zones[0]) {
            Ok(out) => out,
            Err(_) => return Ok(None),
        };
        if !probe.contains("SOA") && !probe.contains(" PTR ") {
            return Ok(None); // empty answer = transfer refused
        }
        let mut facts = parse_axfr(&probe, range);
        on_progress(1.0 / n as f32, &format!("AXFR 1/{n} zones"));

        if n > 1 {
            let par = self.concurrency.clamp(1, 8);
            let args = zones[1..].join(" ");
            let remote = format!(
                "printf '%s\\n' {args} | xargs -P{par} -n1 sh -c 'dig +noall +answer AXFR \"$0\" @{srv} 2>/dev/null | sed \"s/^/R /\"; printf \"T\\n\"'",
                srv = self.axfr_server
            );
            let mut done = 1usize;
            let mut results = String::new();
            self.vantage.run_streaming(&remote, |line| {
                if line == "T" {
                    done += 1;
                    on_progress(done as f32 / n as f32, &format!("AXFR {done}/{n} zones"));
                } else if let Some(rest) = line.strip_prefix("R ") {
                    results.push_str(rest);
                    results.push('\n');
                }
            })?;
            facts.extend(parse_axfr(&results, range));
        }
        Ok(Some(facts))
    }

    /// Transfer a single reverse zone, returning dig's answer section (records only).
    fn axfr_zone(&self, zone: &str) -> anyhow::Result<String> {
        let remote = format!("dig +noall +answer AXFR {zone} @{}", self.axfr_server);
        self.vantage.run(&remote)
    }
}

/// The `/24` reverse zones (`c.b.a.in-addr.arpa`) that `range` overlaps, aligned down to
/// `/24` boundaries. Empty when the range would need more than [`MAX_ZONES`] zones (the
/// signal to skip AXFR and sweep instead).
fn reverse_zones(range: &Cidr) -> Vec<String> {
    // IPv4 `in-addr.arpa` only; IPv6 `ip6.arpa` transfer is a follow-up.
    let IpAddr::V4(net) = range.network() else {
        return Vec::new();
    };
    let start = u64::from(u32::from(net));
    let end = start + range.block_len() as u64; // exclusive (v4 block_len fits u64)
    let first = start & !0xFF; // align down to a /24 boundary
    let count = (end - first).div_ceil(256) as usize;
    if count > MAX_ZONES {
        return Vec::new();
    }
    (0..count)
        .map(|i| {
            let o = ((first + (i as u64) * 256) as u32).to_be_bytes(); // [a, b, c, d]
            format!("{}.{}.{}.in-addr.arpa", o[2], o[1], o[0])
        })
        .collect()
}

/// Map a reverse-DNS owner name (`1.3.87.10.in-addr.arpa.`) back to its address
/// (`10.87.3.1`) — the four labels are the octets in reverse.
fn ptr_owner_to_ip(owner: &str) -> Option<std::net::Ipv4Addr> {
    let labels = owner.trim_end_matches('.').strip_suffix(".in-addr.arpa")?;
    let parts: Vec<u8> = labels.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    match parts[..] {
        [d, c, b, a] => Some(std::net::Ipv4Addr::new(a, b, c, d)),
        _ => None,
    }
}

/// Parse `dig +answer` PTR lines from an AXFR into facts, keeping only those in `range`.
///
/// Each PTR line is `<owner> <ttl> <class> PTR <target>`; we map the owner back to its
/// address and keep the target as the name. Non-PTR records (SOA, NS, …) are skipped.
#[must_use]
pub fn parse_axfr(output: &str, range: &Cidr) -> Vec<AddressFacts> {
    let mut out = Vec::new();
    for line in output.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let Some(pi) = f.iter().position(|&t| t == "PTR") else {
            continue;
        };
        let (Some(owner), Some(target)) = (f.first(), f.get(pi + 1)) else {
            continue;
        };
        let Some(v4) = ptr_owner_to_ip(owner) else {
            continue;
        };
        let addr = IpAddr::V4(v4);
        if !range.contains(addr) {
            continue;
        }
        out.push(AddressFacts { addr, netbox: None, ptr: Some((*target).to_string()), live: false });
    }
    out
}

/// The space-separated host list for the remote shell loop.
fn host_list(range: &Cidr) -> String {
    range.hosts().map(|a| a.to_string()).collect::<Vec<_>>().join(" ")
}

/// Parse `"<ip> <ptr>"` lines into `ptr`-only facts.
///
/// How: split each non-empty line into address and name; skip anything that does
/// not parse as an IPv4 address. Only the `ptr` field is set.
#[must_use]
pub fn parse_ptrs(output: &str) -> Vec<AddressFacts> {
    let mut out = Vec::new();
    for line in output.lines() {
        let mut it = line.split_whitespace();
        let (Some(ip), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(addr) = ip.parse() else { continue };
        out.push(AddressFacts {
            addr,
            netbox: None,
            ptr: Some(name.to_string()),
            live: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reverse_sweep_output() {
        let sample = "\
10.87.3.68 dop21-ipmi.nfra.nl.
10.87.3.11 iprotect-keyreader.nfra.nl.
garbage line without ip
10.87.3.90";
        let facts = parse_ptrs(sample);
        assert_eq!(facts.len(), 2); // the garbage and the ip-only line are skipped
        assert_eq!(facts[0].addr, std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 87, 3, 68)));
        assert_eq!(facts[0].ptr.as_deref(), Some("dop21-ipmi.nfra.nl."));
        assert!(facts[0].netbox.is_none() && !facts[0].live);
    }

    #[test]
    fn host_list_covers_usable_hosts() {
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let list = host_list(&range);
        assert!(list.starts_with("10.87.3.1 "));
        assert!(list.ends_with(" 10.87.3.254"));
    }

    #[test]
    fn reverse_zones_cover_the_range_by_slash_24() {
        // A /24 is one zone; a /20 spans its 16 /24s; a /26 still maps to its /24.
        assert_eq!(reverse_zones(&Cidr::parse("10.87.3.0/24").unwrap()), vec!["3.87.10.in-addr.arpa"]);
        assert_eq!(reverse_zones(&Cidr::parse("10.87.3.0/26").unwrap()), vec!["3.87.10.in-addr.arpa"]);
        let z20 = reverse_zones(&Cidr::parse("10.87.0.0/20").unwrap());
        assert_eq!(z20.len(), 16);
        assert_eq!(z20[0], "0.87.10.in-addr.arpa");
        assert_eq!(z20[15], "15.87.10.in-addr.arpa");
        // A /8 needs 65 536 zones — over the cap, so AXFR is declined (empty → sweep).
        assert!(reverse_zones(&Cidr::parse("10.0.0.0/8").unwrap()).is_empty());
    }

    #[test]
    fn ptr_owner_maps_back_to_its_address() {
        let ip = ptr_owner_to_ip("1.3.87.10.in-addr.arpa.").unwrap();
        assert_eq!(ip, std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 87, 3, 1)));
        assert!(ptr_owner_to_ip("nonsense.example.com.").is_none());
    }

    #[test]
    fn parses_axfr_answer_lines() {
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let answer = "\
3.87.10.in-addr.arpa.\t3600\tIN\tSOA\tns.nfra.nl. root.nfra.nl. 1 2 3 4 5
68.3.87.10.in-addr.arpa. 3600 IN PTR dop21-ipmi.nfra.nl.
99.3.99.10.in-addr.arpa. 3600 IN PTR elsewhere.nfra.nl.
3.87.10.in-addr.arpa.\t3600\tIN\tNS\tns.nfra.nl.";
        let facts = parse_axfr(answer, &range);
        // SOA/NS skipped; the out-of-range .99 host dropped; only the /24 PTR kept.
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].addr, std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 87, 3, 68)));
        assert_eq!(facts[0].ptr.as_deref(), Some("dop21-ipmi.nfra.nl."));
    }
}
