// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! DNS as a fact source: the PTR records actually served. This is the most reliable
//! "is it allocated?" signal we found — it caught addresses NetBox never recorded.
//! We run one reverse lookup per host on the vantage (its resolver knows the
//! internal zones) and collect the answers.

use super::{FactSource, Vantage};
use crate::reconcile::{AddressFacts, Cidr};

/// Reverse-resolves every host in a range via the vantage's resolver.
#[derive(Debug, Clone)]
pub struct DnsSource {
    /// A host whose resolver can see the internal reverse zones.
    pub vantage: Vantage,
}

impl FactSource for DnsSource {
    fn gather(&self, range: &Cidr) -> anyhow::Result<Vec<AddressFacts>> {
        let ips = host_list(range);
        // One `host` per address; print "ip name" only when a PTR exists. The
        // trailing `true` guarantees a zero exit even when the last address (or any)
        // has no PTR — otherwise the loop's final `&&` would fail the whole ssh call.
        let remote = format!(
            "for ip in {ips}; do p=$(host \"$ip\" 2>/dev/null | awk '/pointer/{{print $NF}}'); [ -n \"$p\" ] && echo \"$ip $p\"; done; true"
        );
        let out = self.vantage.run(&remote)?;
        Ok(parse_ptrs(&out))
    }
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
        assert_eq!(facts[0].addr, std::net::Ipv4Addr::new(10, 87, 3, 68));
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
}
