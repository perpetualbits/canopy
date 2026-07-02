// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! NetBox as a fact source: the intended IP inventory. We `curl` the REST API from
//! the vantage host (NetBox is internal-only), handing the token over stdin so it
//! never lands in any argv. Only the `dns_name` matters for reconciliation today.

use anyhow::Context;

use super::{FactSource, Vantage};
use crate::reconcile::{AddressFacts, Cidr, NetBoxRecord};

/// Queries NetBox for the IP objects in a prefix.
#[derive(Debug, Clone)]
pub struct NetboxSource {
    /// Where to run `curl` from (NetBox is not reachable off-site).
    pub vantage: Vantage,
    /// Base URL, e.g. `"https://netbox.astron.nl"`.
    pub base_url: String,
    /// API token (read scope is enough). Fed to the remote `curl` via stdin.
    pub token: String,
}

impl FactSource for NetboxSource {
    fn gather(&self, range: &Cidr) -> anyhow::Result<Vec<AddressFacts>> {
        // `parent=<network>/<len>` returns every IP object inside the prefix.
        let url = format!(
            "{}/api/ipam/ip-addresses/?parent={}/{}&limit=1000",
            self.base_url.trim_end_matches('/'),
            range.network(),
            range.prefix_len
        );
        // `read TOK` pulls the token from stdin so it is never a command argument.
        let remote = format!("read TOK; curl -sS --max-time 25 -H \"Authorization: Token $TOK\" '{url}'");
        let json = self
            .vantage
            .run_with_stdin(&remote, &format!("{}\n", self.token))
            .context("querying NetBox from the vantage host")?;
        parse_ip_addresses(&json, range)
    }
}

/// Parse a NetBox `ip-addresses` list response into per-address facts.
///
/// How: read the `results` array; for each object take `address` (drop the `/mask`)
/// and `dns_name` (empty string ⇒ no name). Only the `netbox` field is set here.
/// Addresses outside `range` are ignored defensively.
///
/// # Errors
/// Fails if the body is not the expected JSON shape.
pub fn parse_ip_addresses(json: &str, range: &Cidr) -> anyhow::Result<Vec<AddressFacts>> {
    let v: serde_json::Value = serde_json::from_str(json).context("NetBox response was not JSON")?;
    let results = v
        .get("results")
        .and_then(|r| r.as_array())
        .context("NetBox response had no `results` array")?;

    let mut out = Vec::new();
    for obj in results {
        let Some(addr_str) = obj.get("address").and_then(|a| a.as_str()) else {
            continue;
        };
        let ip_part = addr_str.split('/').next().unwrap_or(addr_str);
        let Ok(addr) = ip_part.parse() else { continue };
        if !range.contains(addr) {
            continue;
        }
        let dns_name = obj
            .get("dns_name")
            .and_then(|d| d.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.push(AddressFacts {
            addr,
            netbox: Some(NetBoxRecord { dns_name }),
            ptr: None,
            live: false,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses_and_names() {
        let json = r#"{
            "count": 3,
            "next": null,
            "results": [
                {"address": "10.87.3.131/20", "dns_name": ""},
                {"address": "10.87.3.68/20",  "dns_name": "dop21-ipmi.nfra.nl"},
                {"address": "10.99.9.9/24",   "dns_name": "elsewhere.nfra.nl"}
            ]
        }"#;
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        let facts = parse_ip_addresses(json, &range).unwrap();
        // The out-of-range .99 address is dropped.
        assert_eq!(facts.len(), 2);

        let by = |o: u8| facts.iter().find(|f| f.addr == std::net::Ipv4Addr::new(10, 87, 3, o)).unwrap();
        assert_eq!(by(131).netbox.as_ref().unwrap().dns_name, None); // empty → None
        assert_eq!(by(68).netbox.as_ref().unwrap().dns_name.as_deref(), Some("dop21-ipmi.nfra.nl"));
        assert!(by(68).ptr.is_none() && !by(68).live); // NetBox sets only its field
    }

    #[test]
    fn rejects_non_json() {
        let range = Cidr::parse("10.87.3.0/24").unwrap();
        assert!(parse_ip_addresses("<html>403</html>", &range).is_err());
    }
}
