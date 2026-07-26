//! orama-core — transparent interception proxy for coding-agent API traffic.
//!
//! This crate holds the reusable logic (config, server, relay, storage). Both
//! front ends are thin wrappers over it: the `cli` crate is a binary, and
//! `apps/desktop` is a Tauri window pointed at the server this crate runs.

pub mod api;
pub mod catalog;
pub mod config;
pub mod connect;
pub mod derive;
pub mod detect;
pub mod parse;
pub mod pricing;
pub mod reconstruct;
pub mod relay;
pub mod server;
pub mod store;
pub mod util;

pub use config::Config;
pub use server::{bind, serve, Bound};
