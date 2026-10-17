//! Transport over the Linux `usbtmc` kernel driver (`/dev/usbtmcN`).
//!
//! The driver turns each `write()` into one DEV_DEP_MSG_OUT and each `read()` into a
//! REQUEST_DEV_DEP_MSG_IN for at most `count` bytes. The EOM bit of the answer is read back
//! with `USBTMC_IOCTL_MSG_IN_ATTR`.
//!
//! Every read request is capped at [`MAX_READ`] so the device's answer fits into the driver's
//! first 4 KiB bulk buffer. Larger transfers are continued by the driver's asynchronous
//! `usbtmc_generic_read`, which over USB/IP returned early (24553 of 84109 screenshot bytes);
//! the rest of the transfer then desynchronised the bulk pipe ("wrong MsgID") and the scope
//! stopped answering until power-cycled.
//!
//! Only `USBTMC_IOCTL_CLEAR` and the halt-clear ioctls are used for recovery. The
//! `ABORT_BULK_*` ioctls hang in `usb_kill_urb` when the device is attached via USB/IP
//! (vhci_hcd), leaving the calling thread in uninterruptible sleep.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::transport::{ReadChunk, Transport};

const RIGOL_VID: &str = "1ab1";

/// `USBTMC_BUFSIZE` (4096) minus the 12-byte header, minus 4 so the first URB is never
/// completely full (a full URB makes the driver continue with `usbtmc_generic_read`).
pub const MAX_READ: usize = 4096 - 12 - 4;

mod ioctl {
    nix::ioctl_none!(clear, b'[', 2);
    nix::ioctl_none!(clear_out_halt, b'[', 6);
    nix::ioctl_none!(clear_in_halt, b'[', 7);
    nix::ioctl_write_ptr!(set_timeout, b'[', 10, u32);
    nix::ioctl_read!(msg_in_attr, b'[', 24, u8);
}

pub struct UsbTmcTransport {
    path: PathBuf,
    file: File,
    timeout: Duration,
}

impl UsbTmcTransport {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut t = UsbTmcTransport {
            path,
            file,
            timeout: Duration::from_secs(2),
        };
        t.set_timeout(t.timeout)?;
        Ok(t)
    }

    /// Opens the first Rigol USBTMC device found (see [`find_usbtmc_devices`]).
    pub fn open_first() -> io::Result<Self> {
        let devices = find_usbtmc_devices();
        let path = devices.first().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no Rigol /dev/usbtmc* device found",
            )
        })?;
        Self::open(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ioctl_result(r: nix::Result<i32>) -> io::Result<()> {
        r.map(|_| ()).map_err(io::Error::from)
    }
}

impl Transport for UsbTmcTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        let len = buf.len().min(MAX_READ);
        let n = self.file.read(&mut buf[..len])?;
        if n == 0 {
            // The driver returns 0 after rejecting a malformed bulk-in header.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "usbtmc: empty read (protocol desync)",
            ));
        }
        let mut attr = 0u8;
        // SAFETY: valid open fd of a usbtmc device; `attr` outlives the call and matches the
        // ioctl's `__u8` argument.
        let end = match unsafe { ioctl::msg_in_attr(self.file.as_raw_fd(), &mut attr) } {
            Ok(_) => attr & 1 != 0,
            Err(_) => n < len,
        };
        Ok(ReadChunk { n, end })
    }

    fn clear(&mut self) -> io::Result<()> {
        let fd = self.file.as_raw_fd();
        // Same sequence that reliably recovered the MHO954 after an unanswered query.
        // SAFETY: argument-less ioctls on a valid open usbtmc fd.
        unsafe {
            Self::ioctl_result(ioctl::clear_out_halt(fd))?;
            Self::ioctl_result(ioctl::clear_in_halt(fd))?;
            Self::ioctl_result(ioctl::clear(fd))
        }
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        // The driver rejects values below 100 ms.
        let ms = timeout.as_millis().clamp(100, u32::MAX as u128) as u32;
        // SAFETY: valid open fd; `ms` is a `u32` as the ioctl expects and outlives the call.
        unsafe { Self::ioctl_result(ioctl::set_timeout(self.file.as_raw_fd(), &ms))? };
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        format!("usbtmc:{}", self.path.display())
    }
}

/// Lists Rigol USBTMC device nodes, preferring the udev symlink `/dev/usbtmc_MHO954`.
pub fn find_usbtmc_devices() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let symlink = PathBuf::from("/dev/usbtmc_MHO954");
    if symlink.exists() {
        found.push(symlink);
    }
    let Ok(entries) = std::fs::read_dir("/sys/class/usbmisc") else {
        return found;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("usbtmc"))
        .collect();
    names.sort();
    for name in names {
        // /sys/class/usbmisc/usbtmcN/device is the interface; its parent holds idVendor.
        let vid_path = format!("/sys/class/usbmisc/{name}/device/../idVendor");
        let is_rigol = std::fs::read_to_string(vid_path)
            .map(|v| v.trim().eq_ignore_ascii_case(RIGOL_VID))
            .unwrap_or(false);
        let dev = PathBuf::from(format!("/dev/{name}"));
        let already = found
            .iter()
            .any(|p| std::fs::canonicalize(p).ok() == Some(dev.clone()));
        if is_rigol && dev.exists() && !already {
            found.push(dev);
        }
    }
    found
}
