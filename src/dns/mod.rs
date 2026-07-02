// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! A small, graph-friendly model of DNS: [`name::DnsName`] (labels that nest),
//! [`record::Record`] (leaves and edges), [`zone::Zone`] (a zone + the server it
//! lives on + a safe edit), and [`serial::SerialScheme`] (correct SOA bumps).
//!
//! Two jobs, one model: it backs the **safe write path** now, and it is shaped to
//! back the **node-graph view** later — names nest via `parent`, CNAME/NS/PTR are
//! edges via [`record::Record::target_name`], and zones-on-servers are the groups.
//!
//! Some of this API (name nesting, record edges, the extra record types, the pure
//! serial bump) exists ahead of its consumer: the write path uses part of it today,
//! the node-graph view will use the rest. We keep the structure rather than delete
//! what we are about to need, so the not-yet-wired surface is allowed here.
#![allow(dead_code)]

pub mod name;
pub mod record;
pub mod serial;
pub mod zone;

pub use name::{reverse_ptr, DnsName};
pub use record::Record;
pub use zone::Zone;
