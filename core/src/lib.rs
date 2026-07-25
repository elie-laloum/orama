//! tracer-core — transparent interception proxy for Claude Code API traffic.
//!
//! This crate holds the reusable logic (config, server, relay, storage) so a
//! later Tauri desktop app can depend on it directly. The `cli` crate is a thin
//! binary wrapper.

pub mod config;
pub mod reconstruct;
pub mod relay;
pub mod server;
pub mod store;
pub mod util;

pub use config::Config;
pub use server::serve;
