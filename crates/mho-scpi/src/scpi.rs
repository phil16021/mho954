use std::time::{Duration, Instant};

use tracing::{debug, trace, warn};

use crate::block::parse_block_header;
use crate::error::{Result, ScpiError, is_timeout_io};
use crate::transport::Transport;

/// Default size of a single read request (transports may cap it further, see
/// [`crate::USBTMC_MAX_READ`]).
pub const DEFAULT_CHUNK: usize = 64 * 1024;

/// Upper bound for a single IEEE block (500 Mpts WORD = 1 GB, plus margin).
pub const MAX_BLOCK: usize = 1_200_000_000;
/// Consecutive empty reads tolerated before a response counts as missing.
const MAX_EMPTY_READS: u32 = 4;

/// A response whose shape was detected from its first byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Line(String),
    Block(Vec<u8>),
}

/// SCPI session on top of a [`Transport`].
///
/// Every I/O error triggers a device clear so the next command starts from a clean state:
/// an unanswered query otherwise blocks the instrument's message exchange until cleared.
pub struct Scpi {
    transport: Box<dyn Transport>,
    rx: Vec<u8>,
    rx_end: bool,
    chunk: usize,
    last_duration: Duration,
}

impl Scpi {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Scpi { transport, rx: Vec::new(), rx_end: false, chunk: DEFAULT_CHUNK, last_duration: Duration::ZERO }
    }

    pub fn describe(&self) -> String {
        self.transport.describe()
    }

    pub fn set_chunk_size(&mut self, chunk: usize) {
        self.chunk = chunk.max(64);
    }

    pub fn timeout(&self) -> Duration {
        self.transport.timeout()
    }

    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        Ok(self.transport.set_timeout(timeout)?)
    }

    /// Runs `f` with a temporarily changed I/O timeout.
    pub fn with_timeout<R>(&mut self, timeout: Duration, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        let old = self.timeout();
        self.set_timeout(timeout)?;
        let r = f(self);
        let restore = self.set_timeout(old);
        let r = r?;
        restore?;
        Ok(r)
    }

    /// Duration of the most recent command/query round trip.
    pub fn last_duration(&self) -> Duration {
        self.last_duration
    }

    /// Sends a command that produces no response.
    pub fn write(&mut self, cmd: &str) -> Result<()> {
        trace!(cmd, "scpi write");
        let start = Instant::now();
        self.discard_stale();
        let r = self.send(cmd);
        self.last_duration = start.elapsed();
        r
    }

    /// Sends `prefix` followed by `data` as an IEEE 488.2 definite-length block.
    pub fn write_block(&mut self, prefix: &str, data: &[u8]) -> Result<()> {
        let start = Instant::now();
        self.discard_stale();
        let len = data.len().to_string();
        let mut msg = Vec::with_capacity(prefix.len() + len.len() + data.len() + 4);
        msg.extend_from_slice(prefix.as_bytes());
        msg.push(b' ');
        msg.extend_from_slice(format!("#{}{}", len.len(), len).as_bytes());
        msg.extend_from_slice(data);
        msg.push(b'\n');
        let r = self.transport.write_all(&msg).map_err(|e| self.fail(prefix, e));
        self.last_duration = start.elapsed();
        r
    }

    /// Sends a query and returns the response line without the trailing newline.
    pub fn query(&mut self, cmd: &str) -> Result<String> {
        let start = Instant::now();
        let r = self.query_inner(cmd);
        self.last_duration = start.elapsed();
        trace!(cmd, ?r, elapsed_ms = self.last_duration.as_secs_f64() * 1e3, "scpi query");
        r
    }

    pub fn query_f64(&mut self, cmd: &str) -> Result<f64> {
        let s = self.query(cmd)?;
        s.trim().parse::<f64>().map_err(|e| ScpiError::parse(cmd, format!("{s:?}: {e}")))
    }

    pub fn query_bool(&mut self, cmd: &str) -> Result<bool> {
        let s = self.query(cmd)?;
        match s.trim().to_ascii_uppercase().as_str() {
            "1" | "ON" => Ok(true),
            "0" | "OFF" => Ok(false),
            other => Err(ScpiError::parse(cmd, format!("expected boolean, got {other:?}"))),
        }
    }

    /// Sends a query and reads either a line or, if the reply starts with `#`, a block.
    pub fn query_auto(&mut self, cmd: &str) -> Result<Response> {
        let start = Instant::now();
        self.discard_stale();
        self.send(cmd)?;
        let r = self.guard(cmd, |s| {
            let mut empty = 0;
            while s.rx.is_empty() {
                if s.fill(s.chunk)? == 0 {
                    if s.rx_end {
                        return Ok(Response::Line(String::new()));
                    }
                    empty += 1;
                    if empty >= MAX_EMPTY_READS {
                        return Err(ScpiError::parse(cmd, "no data in response"));
                    }
                }
            }
            if s.rx[0] == b'#' {
                s.read_block(cmd, &mut |_, _| {}).map(Response::Block)
            } else {
                s.read_line(cmd).map(Response::Line)
            }
        });
        self.last_duration = start.elapsed();
        r
    }

    /// Sends a query whose response is an IEEE 488.2 definite-length block.
    pub fn query_block(&mut self, cmd: &str) -> Result<Vec<u8>> {
        self.query_block_progress(cmd, &mut |_, _| {})
    }

    /// Like [`Self::query_block`], reporting `(received, total)` payload bytes.
    pub fn query_block_progress(
        &mut self,
        cmd: &str,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u8>> {
        let start = Instant::now();
        let r = self.query_block_inner(cmd, progress);
        self.last_duration = start.elapsed();
        if let Ok(data) = &r {
            let secs = self.last_duration.as_secs_f64();
            debug!(cmd, bytes = data.len(), ms = secs * 1e3, mb_s = data.len() as f64 / secs / 1e6, "block read");
        }
        r
    }

    /// Drains the error queue (`:SYSTem:ERRor?`) and returns all non-zero entries.
    pub fn errors(&mut self) -> Result<Vec<(i32, String)>> {
        let mut out = Vec::new();
        for _ in 0..32 {
            let line = self.query(":SYST:ERR?")?;
            let (code, msg) = line.split_once(',').unwrap_or((line.as_str(), ""));
            let code: i32 = code.trim().parse().map_err(|_| ScpiError::parse(":SYST:ERR?", line.clone()))?;
            if code == 0 {
                break;
            }
            out.push((code, msg.trim().trim_matches('"').to_string()));
        }
        Ok(out)
    }

    /// Device clear plus local buffer reset.
    pub fn recover(&mut self) {
        self.rx.clear();
        self.rx_end = false;
        if let Err(e) = self.transport.clear() {
            warn!("device clear failed: {e}");
        }
    }

    fn query_inner(&mut self, cmd: &str) -> Result<String> {
        self.discard_stale();
        self.send(cmd)?;
        let line = self.guard(cmd, |s| s.read_line(cmd))?;
        Ok(line)
    }

    fn query_block_inner(&mut self, cmd: &str, progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<u8>> {
        self.discard_stale();
        self.send(cmd)?;
        self.guard(cmd, |s| s.read_block(cmd, progress))
    }

    fn send(&mut self, cmd: &str) -> Result<()> {
        let mut msg = Vec::with_capacity(cmd.len() + 1);
        msg.extend_from_slice(cmd.as_bytes());
        msg.push(b'\n');
        let r = self.transport.write_all(&msg);
        r.map_err(|e| self.fail(cmd, e))
    }

    /// Runs a read step; on failure recovers the session and maps timeouts.
    fn guard<R>(&mut self, cmd: &str, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        match f(self) {
            Ok(v) => Ok(v),
            Err(ScpiError::Io(e)) => Err(self.fail(cmd, e)),
            Err(e) => {
                self.recover();
                Err(e)
            }
        }
    }

    fn fail(&mut self, cmd: &str, e: std::io::Error) -> ScpiError {
        self.recover();
        if is_timeout_io(&e) { ScpiError::Timeout(cmd.to_string()) } else { ScpiError::Io(e) }
    }

    fn discard_stale(&mut self) {
        if !self.rx.is_empty() {
            debug!(bytes = self.rx.len(), "discarding stale response bytes");
            self.rx.clear();
        }
        self.rx_end = false;
    }

    /// Reads up to `want` more bytes into the receive buffer (the transport may return fewer).
    fn fill(&mut self, want: usize) -> Result<usize> {
        let old = self.rx.len();
        self.rx.resize(old + want.max(1), 0);
        match self.transport.read_some(&mut self.rx[old..]) {
            Ok(c) => {
                self.rx.truncate(old + c.n);
                self.rx_end = c.end;
                Ok(c.n)
            }
            Err(e) => {
                self.rx.truncate(old);
                Err(ScpiError::Io(e))
            }
        }
    }

    fn read_line(&mut self, cmd: &str) -> Result<String> {
        let mut empty = 0;
        loop {
            if let Some(pos) = self.rx.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.rx.drain(..=pos).collect();
                self.rx.clear();
                return Ok(String::from_utf8_lossy(&line[..pos]).trim_end_matches('\r').to_string());
            }
            // A short USBTMC read is not a reliable end-of-message marker (long responses arrive
            // in several transfers), so only the '\n' terminator ends a line.
            if self.rx.len() > 16 * 1024 * 1024 {
                return Err(ScpiError::parse(cmd, "response line exceeds 16 MiB"));
            }
            if self.fill(self.chunk)? == 0 {
                // An empty message (EOM without data) is an empty response.
                if self.rx_end && self.rx.is_empty() {
                    return Ok(String::new());
                }
                empty += 1;
                if empty >= MAX_EMPTY_READS {
                    return Err(ScpiError::parse(cmd, "no data in response"));
                }
            }
        }
    }

    fn read_block(&mut self, cmd: &str, progress: &mut dyn FnMut(usize, usize)) -> Result<Vec<u8>> {
        let mut empty = 0;
        let header = loop {
            match parse_block_header(&self.rx).map_err(|m| ScpiError::parse(cmd, m))? {
                Some(h) => break h,
                None => {
                    if self.fill(self.chunk)? == 0 {
                        empty += 1;
                        if empty >= MAX_EMPTY_READS {
                            return Err(ScpiError::parse(cmd, "response ended inside block header"));
                        }
                    }
                }
            }
        };
        if header.data_len.is_some_and(|l| l > MAX_BLOCK) {
            return Err(ScpiError::parse(cmd, format!("block length {:?} exceeds limit", header.data_len)));
        }

        let Some(len) = header.data_len else {
            return self.read_indefinite_block(header.header_len);
        };

        let total = header.header_len + len;
        self.rx.reserve(total + 1 - self.rx.len().min(total + 1));
        progress(self.rx.len().saturating_sub(header.header_len).min(len), len);
        while self.rx.len() < total {
            // +1 so the final request also covers the trailing '\n'.
            let want = (total - self.rx.len() + 1).min(self.chunk);
            self.fill(want)?;
            progress((self.rx.len() - header.header_len).min(len), len);
        }

        if self.rx.len() == total {
            // Rigol terminates blocks with '\n'; consume it so it cannot leak into the next
            // response. If none arrives the device is cleared, the data is still complete.
            if let Err(e) = self.fill(1) {
                debug!("no block terminator after `{cmd}`: {e}");
                self.recover();
            }
        }
        let mut data = std::mem::take(&mut self.rx);
        data.truncate(total);
        data.drain(..header.header_len);
        self.rx_end = false;
        Ok(data)
    }

    fn read_indefinite_block(&mut self, header_len: usize) -> Result<Vec<u8>> {
        while !self.rx_end {
            self.fill(self.chunk)?;
            if self.rx.len() > MAX_BLOCK {
                return Err(ScpiError::parse("#0 block", "indefinite block exceeds limit"));
            }
        }
        let mut data = std::mem::take(&mut self.rx);
        data.drain(..header_len);
        if data.last() == Some(&b'\n') {
            data.pop();
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;

    #[test]
    fn query_line_and_block() {
        let mock = MockTransport::new()
            .expect("*IDN?", b"RIGOL TECHNOLOGIES,MHO954,X,00.01.00\n")
            .expect(":WAV:DATA?", b"#14abcd\n");
        let mut s = Scpi::new(Box::new(mock.clone()));
        assert_eq!(s.query("*IDN?").unwrap(), "RIGOL TECHNOLOGIES,MHO954,X,00.01.00");
        assert_eq!(s.query_block(":WAV:DATA?").unwrap(), b"abcd");
        assert!(mock.is_done());
    }

    #[test]
    fn block_in_small_chunks() {
        let mut payload = b"#9000000010".to_vec();
        payload.extend_from_slice(b"0123456789\n");
        let mock = MockTransport::new().with_max_read(3).expect(":DISP:DATA? PNG", &payload);
        let mut s = Scpi::new(Box::new(mock));
        let mut calls = 0;
        let data = s.query_block_progress(":DISP:DATA? PNG", &mut |_, total| {
            calls += 1;
            assert_eq!(total, 10);
        });
        assert_eq!(data.unwrap(), b"0123456789");
        assert!(calls > 1);
    }

    #[test]
    fn block_split_into_short_transfers() {
        // The MHO954 sends large blocks in several USB transfers; each read ends "short".
        let mut payload = b"#9000000010".to_vec();
        payload.extend_from_slice(b"0123456789\n");
        let mock = MockTransport::new()
            .with_short_transfers(4)
            .expect(":DISP:DATA? PNG", &payload)
            .expect("*IDN?", b"long line that spans transfers\n");
        let mut s = Scpi::new(Box::new(mock));
        assert_eq!(s.query_block(":DISP:DATA? PNG").unwrap(), b"0123456789");
        assert_eq!(s.query("*IDN?").unwrap(), "long line that spans transfers");
    }

    #[test]
    fn stream_mode_block_terminator_is_consumed() {
        let mock = MockTransport::new()
            .stream_mode()
            .expect(":WAV:DATA?", b"#13xyz\n")
            .expect(":TRIG:STAT?", b"STOP\n");
        let mut s = Scpi::new(Box::new(mock));
        assert_eq!(s.query_block(":WAV:DATA?").unwrap(), b"xyz");
        assert_eq!(s.query(":TRIG:STAT?").unwrap(), "STOP");
    }

    #[test]
    fn timeout_triggers_clear() {
        let mock = MockTransport::new().expect_silence("*OPT?").expect("*IDN?", b"ok\n");
        let mut s = Scpi::new(Box::new(mock.clone()));
        let err = s.query("*OPT?").unwrap_err();
        assert!(err.is_timeout(), "{err:?}");
        assert_eq!(mock.clear_count(), 1);
        assert_eq!(s.query("*IDN?").unwrap(), "ok");
    }

    #[test]
    fn error_queue() {
        let mock = MockTransport::new()
            .expect(":SYST:ERR?", b"-109,\"Missing parameter\"\n")
            .expect(":SYST:ERR?", b"0,\"No error\"\n");
        let mut s = Scpi::new(Box::new(mock));
        assert_eq!(s.errors().unwrap(), vec![(-109, "Missing parameter".to_string())]);
    }
}
