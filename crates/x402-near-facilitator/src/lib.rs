//! Durable HTTP service for facilitating x402 payments on NEAR.

#![forbid(unsafe_code)]
// This crate is a service implementation shared by its two binaries rather
// than a general-purpose library API. Public items remain typed and documented
// where they define operator-facing behavior, but exhaustive per-method API
// prose would obscure the settlement invariants enforced by the tests.
#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

pub mod auth;
pub mod chain;
pub mod config;
pub mod leadership;
pub mod protocol;
pub mod service;
pub mod store;
pub mod telemetry;

/// Build identifier supplied by Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
