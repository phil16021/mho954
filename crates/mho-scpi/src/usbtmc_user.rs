//! USBTMC in user space via `nusb` (detaches the kernel `usbtmc` driver while open).
//!
//! Why not the kernel driver: over USB/IP (vhci_hcd) the MHO954 got out of step whenever a
//! reply was left unread (timeout, killed process, early end of a large transfer). The kernel
//! then discards every reply as "wrong bTag" and only offers CLEAR / clear-halt, which left the
//! scope unresponsive until power-cycled.
//!
//! This implementation keeps the protocol state itself:
//! * at most one REQUEST_DEV_DEP_MSG_IN is outstanding; after a timeout it stays outstanding
//!   and the next reply (whatever its content) is accepted for it instead of issuing a new one,
//! * replies carrying an unknown bTag (left over from another process) and bytes that are not
//!   a valid header are skipped,
//! * bulk-IN transfers are never cancelled while the device may answer (no `usb_kill_urb`),
//! * no CLEAR_FEATURE(ENDPOINT_HALT) is ever sent.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Bulk, Direction, In, Out};
use tracing::{debug, info, warn};

use crate::transport::{ReadChunk, Transport};

const RIGOL_VID: u16 = 0x1ab1;
const USBTMC_CLASS: u8 = 0xfe;
const USBTMC_SUBCLASS: u8 = 0x03;
const HEADER: usize = 12;
const MSG_OUT: u8 = 1;
const MSG_IN: u8 = 2;
/// Payload requested per REQUEST_DEV_DEP_MSG_IN.
const MAX_TRANSFER: usize = 64 * 1024;
/// Size of each bulk-IN buffer submitted to the host controller.
const IN_BUFFER: usize = 16 * 1024;
const IN_QUEUE: usize = 2;

/// State of the device transfer currently being consumed.
#[derive(Debug, Clone, Copy)]
struct Current {
    remaining: usize,
    padding: usize,
    eom: bool,
}

struct Endpoints {
    out: nusb::Endpoint<Bulk, Out>,
    inp: nusb::Endpoint<Bulk, In>,
}

pub struct UsbTmcUserTransport {
    device: nusb::Device,
    interface_number: u8,
    /// `None` only during drop (endpoints keep the interface claimed).
    eps: Option<Endpoints>,
    rx: VecDeque<u8>,
    tag: u8,
    /// bTag of the REQUEST_DEV_DEP_MSG_IN still waiting for its reply.
    outstanding: Option<u8>,
    current: Option<Current>,
    timeout: Duration,
    describe: String,
}

/// A USBTMC device found on the bus.
#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial: Option<String>,
    pub product: Option<String>,
    pub bus_id: String,
    pub address: u8,
}

/// Lists Rigol USBTMC devices.
pub fn list_usb_devices() -> io::Result<Vec<UsbDeviceInfo>> {
    let devices = nusb::list_devices().wait().map_err(io::Error::other)?;
    Ok(devices
        .filter(|d| d.vendor_id() == RIGOL_VID)
        .filter(|d| d.interfaces().any(|i| i.class() == USBTMC_CLASS && i.subclass() == USBTMC_SUBCLASS))
        .map(|d| UsbDeviceInfo {
            vendor_id: d.vendor_id(),
            product_id: d.product_id(),
            serial: d.serial_number().map(str::to_string),
            product: d.product_string().map(str::to_string),
            bus_id: d.bus_id().to_string(),
            address: d.device_address(),
        })
        .collect())
}

impl UsbTmcUserTransport {
    /// Opens the first Rigol USBTMC device, or the one with the given serial number.
    pub fn open(serial: Option<&str>) -> io::Result<Self> {
        let info = nusb::list_devices()
            .wait()
            .map_err(io::Error::other)?
            .filter(|d| d.vendor_id() == RIGOL_VID)
            .find(|d| serial.is_none_or(|s| d.serial_number() == Some(s)))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Rigol USB device found"))?;
        let iface_info = info
            .interfaces()
            .find(|i| i.class() == USBTMC_CLASS && i.subclass() == USBTMC_SUBCLASS)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "device has no USBTMC interface"))?;
        let interface_number = iface_info.interface_number();
        let describe = format!(
            "usb:{}{}",
            info.product_string().unwrap_or("rigol"),
            info.serial_number().map(|s| format!(":{s}")).unwrap_or_default()
        );

        let device = info.open().wait().map_err(io::Error::other)?;
        let interface = device.detach_and_claim_interface(interface_number).wait().map_err(io::Error::other)?;
        let desc = interface
            .descriptor()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no interface descriptor"))?;
        let mut addr_in = None;
        let mut addr_out = None;
        for ep in desc.endpoints() {
            if ep.transfer_type() == TransferType::Bulk {
                match ep.direction() {
                    Direction::In => addr_in = addr_in.or(Some(ep.address())),
                    Direction::Out => addr_out = addr_out.or(Some(ep.address())),
                }
            }
        }
        let (Some(addr_in), Some(addr_out)) = (addr_in, addr_out) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "USBTMC bulk endpoints missing"));
        };
        let ep_in = interface.endpoint::<Bulk, In>(addr_in).map_err(io::Error::other)?;
        let ep_out = interface.endpoint::<Bulk, Out>(addr_out).map_err(io::Error::other)?;
        info!(%describe, interface_number, "claimed USBTMC interface (kernel driver detached)");

        Ok(UsbTmcUserTransport {
            device,
            interface_number,
            eps: Some(Endpoints { out: ep_out, inp: ep_in }),
            rx: VecDeque::new(),
            // Start at a tag far from what the kernel driver or an earlier run likely used.
            tag: (std::process::id() % 200) as u8 + 30,
            outstanding: None,
            current: None,
            timeout: Duration::from_secs(2),
            describe,
        })
    }

    fn eps(&mut self) -> &mut Endpoints {
        self.eps.as_mut().expect("endpoints present until drop")
    }

    fn next_tag(&mut self) -> u8 {
        self.tag = if self.tag >= 255 { 1 } else { self.tag + 1 };
        self.tag
    }

    fn send_bulk(&mut self, msg: Vec<u8>) -> io::Result<()> {
        let timeout = self.timeout;
        let out = &mut self.eps().out;
        out.submit(msg.into());
        match out.wait_next_complete(timeout) {
            Some(c) => c.status.map_err(io::Error::other),
            None => {
                // An OUT transfer the device does not accept; cancel it so the endpoint
                // is usable again.
                out.cancel_all();
                let _ = out.wait_next_complete(Duration::from_secs(1));
                Err(io::Error::new(io::ErrorKind::TimedOut, "usb: bulk OUT timed out"))
            }
        }
    }

    fn request_in(&mut self, len: usize) -> io::Result<()> {
        let tag = self.next_tag();
        let mut msg = vec![MSG_IN, tag, !tag, 0];
        msg.extend_from_slice(&(len as u32).to_le_bytes());
        msg.extend_from_slice(&[0, 0, 0, 0]);
        self.send_bulk(msg)?;
        self.outstanding = Some(tag);
        Ok(())
    }

    /// Receives more bulk-IN data into `rx`, waiting until `deadline`.
    fn fill(&mut self, deadline: Instant) -> io::Result<()> {
        let inp = &mut self.eps().inp;
        while inp.pending() < IN_QUEUE {
            let buf = inp.allocate(IN_BUFFER);
            inp.submit(buf);
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match inp.wait_next_complete(wait) {
            Some(c) => {
                c.status.map_err(io::Error::other)?;
                self.rx.extend(&c.buffer[..c.actual_len]);
                Ok(())
            }
            // The transfer stays queued; data arriving later is picked up by the next call.
            None => Err(io::Error::new(io::ErrorKind::TimedOut, "usb: no response")),
        }
    }

    fn need(&mut self, n: usize, deadline: Instant) -> io::Result<()> {
        while self.rx.len() < n {
            self.fill(deadline)?;
        }
        Ok(())
    }

    fn skip(&mut self, mut n: usize, deadline: Instant) -> io::Result<()> {
        while n > 0 {
            if self.rx.is_empty() {
                self.fill(deadline)?;
            }
            let k = n.min(self.rx.len());
            self.rx.drain(..k);
            n -= k;
        }
        Ok(())
    }

    /// Waits for the reply header belonging to the outstanding request.
    fn await_header(&mut self, deadline: Instant) -> io::Result<Current> {
        let expected = self.outstanding.expect("request outstanding");
        loop {
            self.need(HEADER, deadline)?;
            let h: Vec<u8> = self.rx.iter().take(HEADER).copied().collect();
            let valid = h[0] == MSG_IN && h[2] == !h[1] && h[3] == 0;
            if !valid {
                // Leftover payload of a transfer we never saw the header of: drop what has
                // arrived so far and resynchronise on the next transfer.
                warn!(bytes = self.rx.len(), "usb: discarding unframed bulk-IN data");
                self.rx.clear();
                continue;
            }
            let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
            let padding = (4 - (HEADER + size) % 4) % 4;
            if h[1] != expected {
                debug!(tag = h[1], expected, size, "usb: skipping stale reply");
                self.rx.drain(..HEADER);
                self.skip(size + padding, deadline)?;
                continue;
            }
            self.rx.drain(..HEADER);
            self.outstanding = None;
            return Ok(Current { remaining: size, padding, eom: h[8] & 1 != 0 });
        }
    }
}

impl Transport for UsbTmcUserTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        // A reply in progress belongs to an abandoned query; forget it.
        self.current = None;
        let tag = self.next_tag();
        let mut msg = Vec::with_capacity(HEADER + data.len() + 3);
        msg.extend_from_slice(&[MSG_OUT, tag, !tag, 0]);
        msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
        msg.extend_from_slice(&[1, 0, 0, 0]); // EOM
        msg.extend_from_slice(data);
        while msg.len() % 4 != 0 {
            msg.push(0);
        }
        self.send_bulk(msg)
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        let deadline = Instant::now() + self.timeout;
        let mut cur = match self.current {
            Some(c) if c.remaining > 0 => c,
            _ => {
                if self.outstanding.is_none() {
                    self.request_in(MAX_TRANSFER)?;
                }
                self.await_header(deadline)?
            }
        };
        if cur.remaining == 0 {
            self.skip(cur.padding, deadline)?;
            self.current = None;
            return Ok(ReadChunk { n: 0, end: cur.eom });
        }
        if self.rx.is_empty() {
            self.current = Some(cur);
            self.fill(deadline)?;
        }
        let n = buf.len().min(cur.remaining).min(self.rx.len());
        for (dst, src) in buf[..n].iter_mut().zip(self.rx.drain(..n)) {
            *dst = src;
        }
        cur.remaining -= n;
        let mut end = false;
        if cur.remaining == 0 {
            self.skip(cur.padding, deadline)?;
            self.current = None;
            end = cur.eom;
        } else {
            self.current = Some(cur);
        }
        Ok(ReadChunk { n, end })
    }

    /// Local resynchronisation only. The device keeps any outstanding request; its reply is
    /// accepted (or skipped) by the next read. No class requests, no clear-halt.
    fn clear(&mut self) -> io::Result<()> {
        self.current = None;
        self.rx.clear();
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }
}

impl Drop for UsbTmcUserTransport {
    fn drop(&mut self) {
        // Only idle IN buffers can be pending here unless a reply is still due.
        if self.outstanding.is_some() {
            let _ = self.fill(Instant::now() + Duration::from_millis(500));
        }
        if let Some(mut eps) = self.eps.take() {
            eps.inp.cancel_all();
            while eps.inp.pending() > 0 && eps.inp.wait_next_complete(Duration::from_millis(500)).is_some() {}
        }
        // Hand the interface back to the kernel driver so /dev/usbtmcN reappears.
        if let Err(e) = self.device.attach_kernel_driver(self.interface_number) {
            debug!("re-attaching kernel driver: {e}");
        }
    }
}
