//! smoltcp <-> tokio bridge device.
//!
//! A synchronous smoltcp [`Device`] backed by mpsc channels of raw IP packets,
//! plus the [`Pumpable`] hook the stack loop uses to move queued inbound packets
//! into the device's RX queue before each poll.

use std::collections::VecDeque;

use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use tokio::sync::mpsc;

use super::STACK_MTU;

/// A smoltcp [`Device`] backed by mpsc channels of raw IP packets.
///
/// `receive`/`transmit` are synchronous (as smoltcp requires); the async TUN
/// (or a test harness) pushes inbound packets and consumes outbound packets via
/// the channels. The stack loop calls [`ChannelDevice::pump_inbound`] before
/// each poll to move queued packets from the channel into the synchronous RX
/// queue.
pub struct ChannelDevice {
    inbound_rx: mpsc::Receiver<Vec<u8>>,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    rx_queue: VecDeque<Vec<u8>>,
}

impl ChannelDevice {
    pub fn new(inbound_rx: mpsc::Receiver<Vec<u8>>, outbound_tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            inbound_rx,
            outbound_tx,
            rx_queue: VecDeque::new(),
        }
    }

    /// Move any packets currently available on the inbound channel into the
    /// synchronous RX queue. Returns the number of packets moved.
    fn pump_inbound(&mut self) -> usize {
        let mut moved = 0;
        while let Ok(pkt) = self.inbound_rx.try_recv() {
            self.rx_queue.push_back(pkt);
            moved += 1;
        }
        moved
    }
}

pub struct ChannelRxToken {
    buffer: Vec<u8>,
}

impl phy::RxToken for ChannelRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer)
    }
}

pub struct ChannelTxToken {
    outbound_tx: mpsc::Sender<Vec<u8>>,
}

impl phy::TxToken for ChannelTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);
        // Best effort: if the consumer is gone, drop the packet.
        if let Err(e) = self.outbound_tx.try_send(buf) {
            tracing::trace!("dropping outbound packet: {e}");
        }
        result
    }
}

impl Device for ChannelDevice {
    type RxToken<'a> = ChannelRxToken;
    type TxToken<'a> = ChannelTxToken;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let buffer = self.rx_queue.pop_front()?;
        Some((
            ChannelRxToken { buffer },
            ChannelTxToken {
                outbound_tx: self.outbound_tx.clone(),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            outbound_tx: self.outbound_tx.clone(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = STACK_MTU;
        caps
    }
}

/// Allow the engine to ask a device to move any externally-queued packets into
/// its synchronous RX queue before a poll. Real channel-backed devices use this;
/// self-contained mock devices can rely on the default no-op.
pub trait Pumpable {
    fn pump(&mut self) {}
}

impl Pumpable for ChannelDevice {
    fn pump(&mut self) {
        self.pump_inbound();
    }
}
