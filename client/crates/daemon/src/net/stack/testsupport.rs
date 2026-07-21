//! Test-support scaffolding (in-memory device + client interface).
//!
//! These items are always compiled and exposed as `#[doc(hidden)] pub` so that
//! external integration tests in `tests/` — which compile against this crate as
//! a normal dependency and therefore cannot see `#[cfg(test)]` code — can drive
//! the real smoltcp engine against an in-memory device. They are NOT part of the
//! stable public API.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use smoltcp::iface::{Config, Interface};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv4Address, Ipv4Cidr};

use super::{Pumpable, STACK_MTU};

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::iface::SocketSet as TestSocketSet;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::socket::tcp as test_tcp;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::time::Instant as TestInstant;
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub use smoltcp::wire::{IpAddress as TestIpAddress, Ipv4Address as TestIpv4Address};

/// An in-memory loopback smoltcp device used to drive the stack from a test
/// "client" smoltcp interface. Packets written by one side appear on the
/// other.
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
#[derive(Clone)]
pub struct MockDevice {
    // Packets destined for THIS device's RX (written by the peer).
    rx: Arc<Mutex<VecDeque<Vec<u8>>>>,
    // Packets transmitted by THIS device (read by the peer).
    tx: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
impl MockDevice {
    /// Create a connected pair of devices.
    pub fn pair() -> (MockDevice, MockDevice) {
        let a_to_b = Arc::new(Mutex::new(VecDeque::new()));
        let b_to_a = Arc::new(Mutex::new(VecDeque::new()));
        let a = MockDevice {
            rx: b_to_a.clone(),
            tx: a_to_b.clone(),
        };
        let b = MockDevice {
            rx: a_to_b,
            tx: b_to_a,
        };
        (a, b)
    }
}

// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub struct MockRxToken(Vec<u8>);
impl phy::RxToken for MockRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub struct MockTxToken(Arc<Mutex<VecDeque<Vec<u8>>>>);
impl phy::TxToken for MockTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.0.lock().unwrap().push_back(buf);
        r
    }
}

impl Device for MockDevice {
    type RxToken<'a> = MockRxToken;
    type TxToken<'a> = MockTxToken;

    fn receive(&mut self, _t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let pkt = self.rx.lock().unwrap().pop_front()?;
        Some((MockRxToken(pkt), MockTxToken(self.tx.clone())))
    }

    fn transmit(&mut self, _t: Instant) -> Option<Self::TxToken<'_>> {
        Some(MockTxToken(self.tx.clone()))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STACK_MTU;
        caps
    }
}

// The mock device's queues are self-contained, so pumping is a no-op.
impl Pumpable for MockDevice {}

/// Build a client-side smoltcp [`Interface`] bound to `client_ip` and routed at
/// the virtual gateway, suitable for connecting to a VIP through a [`MockDevice`].
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub fn client_iface(device: &mut MockDevice, client_ip: Ipv4Address) -> Interface {
    let config = Config::new(HardwareAddress::Ip);
    let mut iface = Interface::new(config, device, Instant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(client_ip, 16)));
    });
    let gw = crate::net::virtual_ip::gateway_ip();
    let _ = iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::from_bytes(&gw.octets()));
    iface
}
