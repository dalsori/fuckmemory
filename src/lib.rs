//! fuckmemory — one local memory shared by every AI coding agent you use.
//!
//! Layers, bottom up:
//!
//! - [`db`]        SQLite schema: raw episodes + a bi-temporal fact graph
//! - [`embed`]     static embeddings, int8-quantized, brute-force SIMD scan
//! - [`fast`]      an mmap'd model cache, so a cold invocation costs ~1 ms
//! - [`store`]     the LLM-free write path
//! - [`retrieve`]  BM25 + vector + graph expansion, fused with RRF, MMR-deduped
//! - [`pack`]      token-budgeted rendering of what the agent will actually read
//! - [`mcp`]       stdio JSON-RPC server, the universal agent interface
//! - [`hook`]      autosave and auto-recall, for when the agent doesn't ask
//! - [`tui`]       the interactive settings screen
//! - [`install`]   detects installed agents and wires itself into each one

pub mod config;
pub mod consolidate;
pub mod db;
pub mod embed;
pub mod fast;
pub mod graph;
pub mod hook;
pub mod install;
pub mod mcp;
pub mod pack;
pub mod retrieve;
pub mod scope;
pub mod store;
pub mod tui;

pub use config::Config;
