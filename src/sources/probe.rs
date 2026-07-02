// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A live-probe fact source: which addresses answer on the wire. This catches
//! squatters that appear in neither NetBox nor DNS. The probe host must sit on the
//! target L2 (so a `ping` triggers ARP), e.g. takkie for `10.87.0.0/20`.

use super::{FactSource, Vantage};
use crate::reconcile::{AddressFacts, Cidr};

/// Pings every host in a range from an on-subnet vantage.
#[derive(Debug, Clone)]
pub struct ProbeSource {
    /// A host on the same L2 as the target range.
    pub vantage: Vantage,
}

impl FactSource for ProbeSource {
    fn gather(&self, range: &Cidr) -> anyhow::Result<Vec<AddressFacts>> {
        let ips = range.hosts().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
        // Fire all pings in parallel (`&` … `wait`) so a /24 finishes in ~1s, not
        // the ~254s a serial 1-second-timeout sweep would take. Print each responder.
        let remote = format!(
            "for ip in {ips}; do (ping -c1 -W1 \"$ip\" >/dev/null 2>&1 && echo \"$ip\") & done; wait; true"
        );
        let out = self.vantage.run(&remote)?;
        Ok(parse_live(&out))
    }
}

/// Parse a list of responding addresses (one IPv4 per line) into `live` facts.
#[must_use]
pub fn parse_live(output: &str) -> Vec<AddressFacts> {
    output
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .map(|addr| AddressFacts {
            addr,
            netbox: None,
            ptr: None,
            live: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_responders() {
        let sample = "10.87.3.90\n10.87.3.68\n\nnot-an-ip\n";
        let facts = parse_live(sample);
        assert_eq!(facts.len(), 2);
        assert!(facts.iter().all(|f| f.live && f.ptr.is_none() && f.netbox.is_none()));
        assert_eq!(facts[0].addr, std::net::Ipv4Addr::new(10, 87, 3, 90));
    }
}
