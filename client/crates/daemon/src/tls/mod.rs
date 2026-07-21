//! TLS support for the local overlay network.
//!
//! Modules (added incrementally):
//!   `ca`    — local CA generation and cert persistence
//!   `trust` — OS trust store installation
//!   `stack` — rustls ServerConfig wiring into the smoltcp proxy (next)

pub mod ca;
pub mod stack;
pub mod trust;

pub use ca::LocalCa;
