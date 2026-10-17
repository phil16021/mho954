//! USBTMC in user space via `nusb` (detaches the kernel `usbtmc` driver while open).
//!
//! Why not the kernel driver: over USB/IP (vhci_hcd) the MHO954 got out of step whenever a
//! reply was left unread (timeout, killed process, early end of a large transfer). The kernel
//! then discards every reply as "wrong bTag" and only offers CLEAR / clear-halt, which left the
//! scope unresponsive until power-cycled.
//!
//! This implementation keeps the protocol state itself:
//! * at most one REQUEST_DEV_DEP_MSG_IN is outstanding; after a timeout it stays outstanding
//!   and the next reply is accepted for it instead of issuing a new request,
//! * replies carrying an unknown bTag (left over from another process) are skipped, and bytes
//!   that are not a valid header are dropped up to the end of their USB transfer,
//! * a message that is still being received is always drained to its end (EOM) before the
//!   next command or a device clear — a clear in the middle of a block reply wedged the scope,
//! * the device clear (INITIATE_CLEAR / CHECK_CLEAR_STATUS) is only sent when a request went
//!   unanswered: after an unknown query the MHO954 answers nothing until it gets one,
//! * bulk-IN transfers are not cancelled while the device may still answer.
//!
//! The USB access itself sits behind [`UsbBackend`] so the protocol logic is testable
//! against a simulated device.

use std::collections::VecDeque;
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Bulk, ControlIn, ControlType, Direction, In, Out, Recipient, TransferError};
use tracing::{debug, info, warn};

use crate::transport::{ReadChunk, Transport};

const RIGOL_VID: u16 = 0x1ab1;
/// MHO900 series. Other Rigol instruments are refused unless explicitly allowed.
pub const MHO900_PID: u16 = 0x0452;
const USBTMC_CLASS: u8 = 0xfe;
const USBTMC_SUBCLASS: u8 = 0x03;
const HEADER: usize = 12;
const MSG_OUT: u8 = 1;
const MSG_IN: u8 = 2;
/// Payload requested per REQUEST_DEV_DEP_MSG_IN. A 4 MiB request once went unanswered.
const MAX_TRANSFER: usize = 64 * 1024;
/// Size of each bulk-IN buffer: one full transfer plus header, rounded to 512-byte packets.
const IN_BUFFER: usize = 64 * 1024 + 512;
const IN_QUEUE: usize = 2;
/// Upper bound for draining an unfinished message before a new command / clear / close.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

const INITIATE_CLEAR: u8 = 5;
const CHECK_CLEAR_STATUS: u8 = 6;
const STATUS_SUCCESS: u8 = 0x01;
const STATUS_PENDING: u8 = 0x02;

// ---- protocol framing (pure functions) ------------------------------------------------------

/// Builds a DEV_DEP_MSG_OUT transfer (always with EOM) including alignment padding.
pub(crate) fn encode_msg_out(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(HEADER + data.len() + 3);
    msg.extend_from_slice(&[MSG_OUT, tag, !tag, 0]);
    msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
    msg.extend_from_slice(&[1, 0, 0, 0]);
    msg.extend_from_slice(data);
    while msg.len() % 4 != 0 {
        msg.push(0);
    }
    msg
}

/// Builds a REQUEST_DEV_DEP_MSG_IN for at most `len` bytes.
pub(crate) fn encode_request_in(tag: u8, len: usize) -> Vec<u8> {
    let mut msg = vec![MSG_IN, tag, !tag, 0];
    msg.extend_from_slice(&(len as u32).to_le_bytes());
    msg.extend_from_slice(&[0, 0, 0, 0]);
    msg
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InHeader {
    pub tag: u8,
    pub size: usize,
    pub eom: bool,
}

/// Parses a DEV_DEP_MSG_IN header; `None` if the bytes are not a valid header.
pub(crate) fn parse_in_header(h: &[u8]) -> Option<InHeader> {
    if h.len() < HEADER || h[0] != MSG_IN || h[2] != !h[1] || h[3] != 0 {
        return None;
    }
    let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    Some(InHeader {
        tag: h[1],
        size,
        eom: h[8] & 1 != 0,
    })
}

fn padding_for(size: usize) -> usize {
    (4 - (HEADER + size) % 4) % 4
}

/// Process-wide bTag counter, so a reconnect never reuses the tags of the previous session.
fn next_tag() -> u8 {
    static TAG: OnceLock<AtomicU8> = OnceLock::new();
    let counter = TAG.get_or_init(|| {
        let seed = std::process::id() as u64
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
        AtomicU8::new((seed % 254) as u8 + 1)
    });
    loop {
        let t = counter.fetch_add(1, Ordering::Relaxed);
        if t != 0 {
            return t;
        }
    }
}

// ---- backend --------------------------------------------------------------------------------

/// Raw USB operations needed by the USBTMC protocol.
pub trait UsbBackend: Send {
    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> io::Result<()>;
    /// Data of the next completed bulk-IN transfer. `Ok(None)` on timeout — the transfer stays
    /// queued and its data is returned by a later call. A stall is reported as
    /// [`io::ErrorKind::BrokenPipe`].
    fn bulk_in(&mut self, deadline: Instant) -> io::Result<Option<Vec<u8>>>;
    /// Class request to the USBTMC interface (device-to-host).
    fn control_in(&mut self, request: u8, length: u16) -> io::Result<Vec<u8>>;
    fn clear_halt_out(&mut self) -> io::Result<()>;
    fn clear_halt_in(&mut self) -> io::Result<()>;
    /// Releases the interface (and re-attaches the kernel driver where applicable).
    fn close(&mut self);
}

struct NusbEndpoints {
    interface: nusb::Interface,
    out: nusb::Endpoint<Bulk, Out>,
    inp: nusb::Endpoint<Bulk, In>,
}

/// [`UsbBackend`] on top of nusb (Linux usbfs).
pub struct NusbBackend {
    device: nusb::Device,
    interface_number: u8,
    eps: Option<NusbEndpoints>,
}

impl NusbBackend {
    fn eps(&mut self) -> io::Result<&mut NusbEndpoints> {
        self.eps
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "usb: closed"))
    }
}

fn transfer_error(e: TransferError) -> io::Error {
    match e {
        TransferError::Stall => io::Error::new(io::ErrorKind::BrokenPipe, "usb: endpoint stalled"),
        TransferError::Disconnected => {
            io::Error::new(io::ErrorKind::NotConnected, "usb: disconnected")
        }
        other => io::Error::other(other),
    }
}

impl UsbBackend for NusbBackend {
    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> io::Result<()> {
        let out = &mut self.eps()?.out;
        out.submit(data.to_vec().into());
        if let Some(c) = out.wait_next_complete(timeout) {
            return c.status.map_err(transfer_error);
        }
        // Not accepted in time: cancel and wait until the cancelled transfer has returned, so
        // its completion cannot be mistaken for the next one.
        out.cancel_all();
        while out.pending() > 0 {
            if out.wait_next_complete(Duration::from_secs(1)).is_none() {
                warn!("usb: cancelled bulk OUT transfer has not returned yet");
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "usb: bulk OUT timed out",
        ))
    }

    fn bulk_in(&mut self, deadline: Instant) -> io::Result<Option<Vec<u8>>> {
        let inp = &mut self.eps()?.inp;
        while inp.pending() < IN_QUEUE {
            let buf = inp.allocate(IN_BUFFER);
            inp.submit(buf);
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        match inp.wait_next_complete(wait) {
            Some(c) => {
                c.status.map_err(transfer_error)?;
                Ok(Some(c.buffer[..c.actual_len].to_vec()))
            }
            None => Ok(None),
        }
    }

    fn control_in(&mut self, request: u8, length: u16) -> io::Result<Vec<u8>> {
        let index = u16::from(self.interface_number);
        let req = ControlIn {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request,
            value: 0,
            index,
            length,
        };
        self.eps()?
            .interface
            .control_in(req, Duration::from_secs(1))
            .wait()
            .map_err(transfer_error)
    }

    fn clear_halt_out(&mut self) -> io::Result<()> {
        self.eps()?
            .out
            .clear_halt()
            .wait()
            .map_err(io::Error::other)
    }

    fn clear_halt_in(&mut self) -> io::Result<()> {
        let inp = &mut self.eps()?.inp;
        inp.cancel_all();
        while inp.pending() > 0 && inp.wait_next_complete(Duration::from_secs(1)).is_some() {}
        inp.clear_halt().wait().map_err(io::Error::other)
    }

    fn close(&mut self) {
        if let Some(mut eps) = self.eps.take() {
            eps.inp.cancel_all();
            while eps.inp.pending() > 0
                && eps
                    .inp
                    .wait_next_complete(Duration::from_millis(500))
                    .is_some()
            {}
        }
        // Interface released above; hand it back to the kernel driver so /dev/usbtmcN returns.
        if let Err(e) = self.device.attach_kernel_driver(self.interface_number) {
            debug!("re-attaching kernel driver: {e}");
        }
    }
}

// ---- device discovery -------------------------------------------------------------------------

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

fn is_usbtmc(d: &nusb::DeviceInfo) -> bool {
    d.vendor_id() == RIGOL_VID
        && d.interfaces()
            .any(|i| i.class() == USBTMC_CLASS && i.subclass() == USBTMC_SUBCLASS)
}

/// Lists Rigol USBTMC devices.
pub fn list_usb_devices() -> io::Result<Vec<UsbDeviceInfo>> {
    let devices = nusb::list_devices().wait().map_err(io::Error::other)?;
    Ok(devices
        .filter(is_usbtmc)
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

impl NusbBackend {
    /// Opens a Rigol USBTMC device: the one with `serial`, otherwise the first one with
    /// product id `pid` (`None` accepts any Rigol USBTMC device).
    pub fn open(serial: Option<&str>, pid: Option<u16>) -> io::Result<(Self, String)> {
        let info = nusb::list_devices()
            .wait()
            .map_err(io::Error::other)?
            .filter(is_usbtmc)
            .filter(|d| serial.is_some() || pid.is_none_or(|p| d.product_id() == p))
            .find(|d| serial.is_none_or(|s| d.serial_number() == Some(s)))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no matching Rigol USBTMC device found",
                )
            })?;
        let interface_number = info
            .interfaces()
            .find(|i| i.class() == USBTMC_CLASS && i.subclass() == USBTMC_SUBCLASS)
            .map(|i| i.interface_number())
            .expect("filtered for USBTMC interface");
        let describe = format!(
            "usb:{}{}",
            info.product_string().unwrap_or("rigol"),
            info.serial_number()
                .map(|s| format!(":{s}"))
                .unwrap_or_default()
        );

        let device = info.open().wait().map_err(io::Error::other)?;
        let interface = device
            .detach_and_claim_interface(interface_number)
            .wait()
            .map_err(io::Error::other)?;
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
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "USBTMC bulk endpoints missing",
            ));
        };
        let inp = interface
            .endpoint::<Bulk, In>(addr_in)
            .map_err(io::Error::other)?;
        let out = interface
            .endpoint::<Bulk, Out>(addr_out)
            .map_err(io::Error::other)?;
        info!(%describe, interface_number, "claimed USBTMC interface (kernel driver detached)");
        let backend = NusbBackend {
            device,
            interface_number,
            eps: Some(NusbEndpoints {
                interface,
                out,
                inp,
            }),
        };
        Ok((backend, describe))
    }
}

// ---- protocol state machine ---------------------------------------------------------------------

/// Received bulk-IN data, kept per USB transfer so unframed bytes can be dropped up to the end
/// of their transfer only.
#[derive(Default)]
struct RxQueue {
    chunks: VecDeque<Vec<u8>>,
    pos: usize,
    len: usize,
}

impl RxQueue {
    fn push(&mut self, chunk: Vec<u8>) {
        if !chunk.is_empty() {
            self.len += chunk.len();
            self.chunks.push_back(chunk);
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn peek(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut pos = self.pos;
        for c in &self.chunks {
            let take = (n - out.len()).min(c.len() - pos);
            out.extend_from_slice(&c[pos..pos + take]);
            pos = 0;
            if out.len() == n {
                break;
            }
        }
        out
    }

    /// Removes up to `n` bytes, copying them into `dst` if given. Returns the count removed.
    fn take(&mut self, n: usize, mut dst: Option<&mut [u8]>) -> usize {
        let mut done = 0;
        while done < n {
            let Some(front) = self.chunks.front() else {
                break;
            };
            let k = (n - done).min(front.len() - self.pos);
            if let Some(d) = dst.as_deref_mut() {
                d[done..done + k].copy_from_slice(&front[self.pos..self.pos + k]);
            }
            self.pos += k;
            done += k;
            if self.pos == front.len() {
                self.chunks.pop_front();
                self.pos = 0;
            }
        }
        self.len -= done;
        done
    }

    /// Drops the rest of the transfer at the front.
    fn drop_front_transfer(&mut self) -> usize {
        match self.chunks.pop_front() {
            Some(c) => {
                let n = c.len() - self.pos;
                self.len -= n;
                self.pos = 0;
                n
            }
            None => 0,
        }
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.pos = 0;
        self.len = 0;
    }
}

/// State of the device transfer currently being consumed.
#[derive(Debug, Clone, Copy)]
struct Current {
    remaining: usize,
    padding: usize,
    eom: bool,
}

/// USBTMC protocol on top of any [`UsbBackend`].
pub struct UsbTmc<B: UsbBackend> {
    backend: B,
    rx: RxQueue,
    /// bTag of the REQUEST_DEV_DEP_MSG_IN still waiting for its reply header.
    outstanding: Option<u8>,
    current: Option<Current>,
    /// EOM flag of the last transfer header received; `false` = message not finished.
    last_eom: bool,
    timeout: Duration,
    describe: String,
}

/// The default USB transport.
pub type UsbTmcUserTransport = UsbTmc<NusbBackend>;

impl UsbTmc<NusbBackend> {
    /// Opens the MHO900 (`pid` 0x0452) or, with `serial`, that specific Rigol instrument.
    pub fn open(serial: Option<&str>) -> io::Result<Self> {
        let (backend, describe) = NusbBackend::open(serial, Some(MHO900_PID))?;
        Ok(UsbTmc::with_backend(backend, describe))
    }
}

impl<B: UsbBackend> UsbTmc<B> {
    pub fn with_backend(backend: B, describe: String) -> Self {
        UsbTmc {
            backend,
            rx: RxQueue::default(),
            outstanding: None,
            current: None,
            last_eom: true,
            timeout: Duration::from_secs(2),
            describe,
        }
    }

    /// `true` while a reply message has been started but not received completely.
    pub fn in_message(&self) -> bool {
        self.current.is_some_and(|c| c.remaining > 0) || !self.last_eom
    }

    fn fill(&mut self, deadline: Instant) -> io::Result<()> {
        match self.backend.bulk_in(deadline) {
            Ok(Some(data)) => {
                self.rx.push(data);
                Ok(())
            }
            Ok(None) => Err(io::Error::new(io::ErrorKind::TimedOut, "usb: no response")),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                warn!("usb: bulk-IN stalled, clearing halt");
                self.backend.clear_halt_in()?;
                Err(e)
            }
            Err(e) => Err(e),
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
            n -= self.rx.take(n, None);
        }
        Ok(())
    }

    /// Drops alignment padding if it was sent. The MHO954 omits it; a transfer ends with a
    /// short packet, so padding that was sent is already in `rx`.
    fn drop_padding(&mut self, padding: usize) {
        let k = padding.min(self.rx.len());
        self.rx.take(k, None);
    }

    fn request_in(&mut self, len: usize) -> io::Result<()> {
        let tag = next_tag();
        self.backend
            .bulk_out(&encode_request_in(tag, len), self.timeout)?;
        self.outstanding = Some(tag);
        Ok(())
    }

    /// Waits for the reply header belonging to the outstanding request.
    fn await_header(&mut self, deadline: Instant) -> io::Result<Current> {
        let expected = self.outstanding.expect("request outstanding");
        loop {
            self.need(HEADER, deadline)?;
            let Some(h) = parse_in_header(&self.rx.peek(HEADER)) else {
                // Remains of a transfer whose header we never saw: drop it up to the end of
                // its USB transfer and resynchronise on the next one.
                let n = self.rx.drop_front_transfer();
                warn!(bytes = n, "usb: discarding unframed bulk-IN data");
                continue;
            };
            self.rx.take(HEADER, None);
            let padding = padding_for(h.size);
            if h.tag != expected {
                debug!(
                    tag = h.tag,
                    expected,
                    size = h.size,
                    "usb: skipping stale reply"
                );
                self.skip(h.size, deadline)?;
                self.drop_padding(padding);
                continue;
            }
            self.outstanding = None;
            self.last_eom = h.eom;
            return Ok(Current {
                remaining: h.size,
                padding,
                eom: h.eom,
            });
        }
    }

    /// Reads and discards the rest of an unfinished message.
    fn drain_message(&mut self, deadline: Instant) -> io::Result<()> {
        let mut drained = 0usize;
        while self.in_message() {
            match self.current {
                Some(c) if c.remaining > 0 => {
                    self.skip(c.remaining, deadline)?;
                    drained += c.remaining;
                    self.drop_padding(c.padding);
                    self.current = None;
                }
                _ => {
                    if self.outstanding.is_none() {
                        self.request_in(MAX_TRANSFER)?;
                    }
                    self.current = Some(self.await_header(deadline)?);
                }
            }
        }
        if drained > 0 {
            debug!(bytes = drained, "usb: drained unfinished reply");
        }
        Ok(())
    }

    /// USBTMC device clear (USBTMC 1.0, 4.2.1.6/4.2.1.7) plus the bulk-OUT halt clear the
    /// specification requires afterwards.
    fn device_clear(&mut self) -> io::Result<()> {
        let st = self.backend.control_in(INITIATE_CLEAR, 1)?;
        if st.first() != Some(&STATUS_SUCCESS) {
            return Err(io::Error::other(format!("INITIATE_CLEAR status {st:02x?}")));
        }
        let mut done = false;
        for _ in 0..100 {
            let st = self.backend.control_in(CHECK_CLEAR_STATUS, 2)?;
            match st.first() {
                Some(&STATUS_PENDING) => {
                    if st.get(1).is_some_and(|b| b & 1 != 0) {
                        // Device still has bulk-IN data queued: read and discard it.
                        let _ = self.fill(Instant::now() + Duration::from_millis(100));
                        self.rx.clear();
                    } else {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                Some(&STATUS_SUCCESS) => {
                    done = true;
                    break;
                }
                _ => return Err(io::Error::other(format!("CHECK_CLEAR_STATUS {st:02x?}"))),
            }
        }
        if !done {
            return Err(io::Error::other(
                "device clear still pending after 100 status checks",
            ));
        }
        self.backend.clear_halt_out()?;
        info!("usb: device clear done");
        Ok(())
    }
}

impl<B: UsbBackend> Transport for UsbTmc<B> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        // The rest of an earlier reply (e.g. a raw query whose response was not read to the
        // end) would otherwise be delivered as the answer to this command.
        if self.in_message() {
            self.drain_message(Instant::now() + DRAIN_TIMEOUT)?;
        }
        self.backend
            .bulk_out(&encode_msg_out(next_tag(), data), self.timeout)
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
            self.drop_padding(cur.padding);
            self.current = None;
            return Ok(ReadChunk { n: 0, end: cur.eom });
        }
        if self.rx.is_empty() {
            self.current = Some(cur);
            self.fill(deadline)?;
        }
        let want = buf.len().min(cur.remaining).min(self.rx.len());
        let n = self.rx.take(want, Some(&mut buf[..want]));
        cur.remaining -= n;
        let mut end = false;
        if cur.remaining == 0 {
            self.drop_padding(cur.padding);
            self.current = None;
            end = cur.eom;
        } else {
            self.current = Some(cur);
        }
        Ok(ReadChunk { n, end })
    }

    /// Brings the session back to a clean state: an unfinished reply is drained to its end;
    /// a device clear is sent only if a request is still unanswered (never mid-message).
    fn clear(&mut self) -> io::Result<()> {
        if self.in_message() {
            match self.drain_message(Instant::now() + DRAIN_TIMEOUT) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::TimedOut && self.current.is_none() => {
                    // The remainder never arrived; the outstanding request is unanswered.
                }
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("usb: reply could not be drained ({e}); not clearing mid-message"),
                    ));
                }
            }
        }
        self.current = None;
        self.last_eom = true;
        if self.outstanding.is_some() {
            let r = self.device_clear();
            self.outstanding = None;
            self.rx.clear();
            return r;
        }
        Ok(())
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn reports_eom(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }
}

impl<B: UsbBackend> Drop for UsbTmc<B> {
    fn drop(&mut self) {
        // Leave the scope idle: finish a reply that is still being sent.
        if self.in_message()
            && let Err(e) = self.drain_message(Instant::now() + Duration::from_secs(2))
        {
            debug!("usb: drain on close: {e}");
        }
        self.backend.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scpi;
    use std::sync::{Arc, Mutex};

    /// Simulated USBTMC instrument. Each query in `answers` produces a reply message; unknown
    /// queries produce nothing and block every further reply until a device clear.
    #[derive(Default)]
    struct FakeState {
        answers: Vec<(String, Vec<u8>)>,
        /// Reply message bytes not yet sent.
        output: VecDeque<u8>,
        /// Request waiting for data: (tag, max len).
        pending_request: Option<(u8, usize)>,
        /// Completed bulk-IN transfers, split into USB buffers.
        in_queue: VecDeque<Vec<u8>>,
        /// Bytes injected before the next reply.
        inject: Vec<Vec<u8>>,
        pad: bool,
        chunk: usize,
        blocked: bool,
        stall_once: bool,
        clears: usize,
        halts_in: usize,
        commands: Vec<String>,
    }

    #[derive(Clone)]
    struct Fake(Arc<Mutex<FakeState>>);

    impl Fake {
        fn new(answers: &[(&str, &[u8])]) -> Self {
            Fake(Arc::new(Mutex::new(FakeState {
                answers: answers
                    .iter()
                    .map(|(q, a)| (q.to_string(), a.to_vec()))
                    .collect(),
                chunk: 512,
                ..Default::default()
            })))
        }

        fn with(self, f: impl FnOnce(&mut FakeState)) -> Self {
            f(&mut self.0.lock().unwrap());
            self
        }

        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.0.lock().unwrap()
        }
    }

    impl FakeState {
        /// Answers the pending request if there is data.
        fn serve(&mut self) {
            let Some((tag, max)) = self.pending_request else {
                return;
            };
            if self.output.is_empty() {
                return;
            }
            self.pending_request = None;
            for junk in self.inject.drain(..) {
                self.in_queue.push_back(junk);
            }
            let n = max.min(self.output.len());
            let eom = n == self.output.len();
            let mut t = vec![MSG_IN, tag, !tag, 0];
            t.extend_from_slice(&(n as u32).to_le_bytes());
            t.extend_from_slice(&[u8::from(eom), 0, 0, 0]);
            t.extend(self.output.drain(..n));
            if self.pad {
                while t.len() % 4 != 0 {
                    t.push(0);
                }
            }
            for c in t.chunks(self.chunk) {
                self.in_queue.push_back(c.to_vec());
            }
        }
    }

    impl UsbBackend for Fake {
        fn bulk_out(&mut self, data: &[u8], _timeout: Duration) -> io::Result<()> {
            let mut s = self.state();
            match data[0] {
                MSG_OUT => {
                    let size = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
                    let cmd = String::from_utf8_lossy(&data[HEADER..HEADER + size])
                        .trim()
                        .to_string();
                    s.commands.push(cmd.clone());
                    if s.blocked {
                        return Ok(());
                    }
                    if cmd.contains('?') {
                        match s
                            .answers
                            .iter()
                            .find(|(q, _)| *q == cmd)
                            .map(|(_, a)| a.clone())
                        {
                            Some(a) => s.output.extend(a),
                            None => s.blocked = true,
                        }
                    }
                }
                MSG_IN => {
                    let max = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
                    s.pending_request = Some((data[1], max));
                }
                _ => panic!("unexpected bulk OUT"),
            }
            if !s.blocked {
                s.serve();
            }
            Ok(())
        }

        fn bulk_in(&mut self, _deadline: Instant) -> io::Result<Option<Vec<u8>>> {
            let mut s = self.state();
            if s.stall_once && !s.in_queue.is_empty() {
                s.stall_once = false;
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stall"));
            }
            Ok(s.in_queue.pop_front())
        }

        fn control_in(&mut self, request: u8, _length: u16) -> io::Result<Vec<u8>> {
            let mut s = self.state();
            match request {
                INITIATE_CLEAR => {
                    s.clears += 1;
                    s.blocked = false;
                    s.output.clear();
                    s.pending_request = None;
                    s.in_queue.clear();
                    Ok(vec![STATUS_SUCCESS])
                }
                CHECK_CLEAR_STATUS => Ok(vec![STATUS_SUCCESS, 0]),
                _ => panic!("unexpected control request"),
            }
        }

        fn clear_halt_out(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_halt_in(&mut self) -> io::Result<()> {
            self.state().halts_in += 1;
            Ok(())
        }

        fn close(&mut self) {}
    }

    fn scpi(fake: &Fake) -> Scpi {
        Scpi::new(Box::new(UsbTmc::with_backend(fake.clone(), "fake".into())))
    }

    const IDN: &[u8] = b"RIGOL TECHNOLOGIES,MHO954,X,00.01.00\n";

    #[test]
    fn framing_roundtrip() {
        let out = encode_msg_out(7, b"*IDN?\n");
        assert_eq!(out.len() % 4, 0);
        assert_eq!(&out[..4], &[MSG_OUT, 7, !7u8, 0]);
        let req = encode_request_in(9, 65536);
        assert_eq!(req.len(), 12);
        let mut h = vec![MSG_IN, 9, !9u8, 0];
        h.extend_from_slice(&50u32.to_le_bytes());
        h.extend_from_slice(&[1, 0, 0, 0]);
        assert_eq!(
            parse_in_header(&h),
            Some(InHeader {
                tag: 9,
                size: 50,
                eom: true
            })
        );
        assert_eq!(parse_in_header(b"#9000000010abc"), None);
        assert_eq!(padding_for(50), 2);
    }

    #[test]
    fn tags_never_zero_and_unique_in_sequence() {
        let tags: Vec<u8> = (0..600).map(|_| next_tag()).collect();
        assert!(tags.iter().all(|&t| t != 0));
        assert!(tags.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn query_without_padding() {
        let fake = Fake::new(&[("*IDN?", IDN)]);
        let mut s = scpi(&fake);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn query_with_padding() {
        let fake = Fake::new(&[("*IDN?", IDN)]).with(|s| s.pad = true);
        let mut s = scpi(&fake);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn block_over_several_transfers() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut reply = format!("#9{:09}", payload.len()).into_bytes();
        reply.extend_from_slice(&payload);
        reply.push(b'\n');
        let fake =
            Fake::new(&[(":DISP:DATA? PNG", &reply), ("*IDN?", IDN)]).with(|s| s.chunk = 4096);
        let mut s = scpi(&fake);
        assert_eq!(s.query_block(":DISP:DATA? PNG").unwrap(), payload);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn stale_reply_from_previous_process_is_skipped() {
        let mut stale = vec![MSG_IN, 172, !172u8, 0];
        stale.extend_from_slice(&5u32.to_le_bytes());
        stale.extend_from_slice(&[1, 0, 0, 0]);
        stale.extend_from_slice(b"old!\n");
        let fake = Fake::new(&[("*IDN?", IDN)]).with(|s| s.inject.push(stale));
        let mut s = scpi(&fake);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn unframed_garbage_before_header_is_dropped_per_transfer() {
        let fake = Fake::new(&[("*IDN?", IDN)])
            .with(|s| s.inject.push(vec![0xde, 0xad, 0xbe, 0xef, 0x42]));
        let mut s = scpi(&fake);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn unknown_query_clears_and_recovers() {
        let fake = Fake::new(&[("*IDN?", IDN)]);
        let mut s = scpi(&fake);
        assert!(s.query(":WLAN:SSID?").unwrap_err().is_timeout());
        assert_eq!(fake.state().clears, 1);
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
    }

    #[test]
    fn unread_multiline_reply_is_drained_before_next_command() {
        // A raw query whose reply is longer than what the caller reads (binary block read as a
        // line): the remainder must not become the answer to the next query.
        let mut reply = b"#15ab\ncd\n".to_vec();
        reply.extend_from_slice(&[b'x'; 70_000]);
        reply.push(b'\n');
        let fake = Fake::new(&[(":SYST:SET?", &reply), ("*IDN?", IDN)]);
        let mut s = scpi(&fake);
        assert_eq!(s.query(":SYST:SET?").unwrap(), "#15ab");
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
        assert_eq!(fake.state().clears, 0, "draining needs no device clear");
    }

    #[test]
    fn clear_mid_message_drains_instead_of_clearing() {
        let mut reply = vec![b'y'; 150_000];
        reply.push(b'\n');
        let fake = Fake::new(&[(":BIG?", &reply)]);
        let mut t = UsbTmc::with_backend(fake.clone(), "fake".into());
        t.write_all(b":BIG?\n").unwrap();
        let mut buf = [0u8; 100];
        t.read_some(&mut buf).unwrap();
        assert!(t.in_message());
        t.clear().unwrap();
        assert!(!t.in_message());
        assert_eq!(fake.state().clears, 0);
    }

    #[test]
    fn stall_clears_in_halt() {
        let fake = Fake::new(&[("*IDN?", IDN)]).with(|s| s.stall_once = true);
        let mut s = scpi(&fake);
        assert!(s.query("*IDN?").is_err());
        assert_eq!(fake.state().halts_in, 1);
    }
}
