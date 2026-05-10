// SPDX-License-Identifier: Apache-2.0

//! `NodeDbRemote` — pgwire client that translates `NodeDb` trait calls
//! into SQL/DSL and sends them to the NodeDB Origin.
//!
//! Split into per-concern files: connection lifecycle and trait impl in
//! `client`, SQL/param translation seams in `sql`, JSON response parsing
//! in `parse`. Each file holds the unit tests for the code it owns.

pub mod client;
mod parse;
mod sql;

pub use client::NodeDbRemote;
