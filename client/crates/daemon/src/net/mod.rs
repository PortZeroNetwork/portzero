//! Virtual overlay network: "Port 0 + Virtual Mesh" implementation.
//!
//! Core idea:
//! - A service sets `PZ_TUNNEL=my-db.portzero.local` (full domain name)
//!   and binds to port 0.
//! - Discovery routes it to overlay because of the `.portzero.local` suffix.
//! - We discover the real ephemeral host port.
//! - We assign a stable virtual IP from 10.254.0.0/16.
//! - We serve DNS for the full name under .portzero.local (scoped).
//! - TUN + user-space stack proxies to the real address.
//!
//! This makes `my-db.portzero.local:5432` work even though the real listener
//! is on a random port.

pub mod dns;
pub mod overlay;
pub mod resolver_config;
pub mod service_table;
pub mod stack;
pub mod tun_device;
pub mod virtual_ip;

pub use service_table::{NetworkService, ServiceTable};
pub use virtual_ip::VirtualIpAllocator;
