//! Scripted transport for tests: each written command must match the next expectation.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::transport::{ReadChunk, Transport};

#[derive(Debug)]
struct Expectation {
    cmd: String,
    response: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct State {
    script: VecDeque<Expectation>,
    pending: Vec<u8>,
    clears: usize,
    log: Vec<String>,
}

#[derive(Clone, Default)]
pub struct MockTransport {
    state: Arc<Mutex<State>>,
    max_read: Option<usize>,
    stream: bool,
    short_transfers: bool,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Query `cmd` answered with `response` (include the trailing `\n`).
    pub fn expect(self, cmd: &str, response: &[u8]) -> Self {
        self.push(cmd, Some(response.to_vec()))
    }

    /// Command without response.
    pub fn expect_write(self, cmd: &str) -> Self {
        self.push(cmd, None)
    }

    /// Query that the device never answers (reads time out).
    pub fn expect_silence(self, cmd: &str) -> Self {
        self.push(cmd, Some(Vec::new()))
    }

    /// Limits each read to `n` bytes to exercise chunked reassembly.
    pub fn with_max_read(mut self, n: usize) -> Self {
        self.max_read = Some(n);
        self
    }

    /// Deliver at most `n` bytes per read and flag every read as a short (ended) transfer,
    /// like the real scope does for responses spanning several USB transfers.
    pub fn with_short_transfers(mut self, n: usize) -> Self {
        self.max_read = Some(n);
        self.short_transfers = true;
        self
    }

    /// Behave like a TCP stream: never report end-of-message.
    pub fn stream_mode(mut self) -> Self {
        self.stream = true;
        self
    }

    pub fn is_done(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.script.is_empty() && s.pending.is_empty()
    }

    pub fn clear_count(&self) -> usize {
        self.state.lock().unwrap().clears
    }

    /// All commands written so far.
    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn push(self, cmd: &str, response: Option<Vec<u8>>) -> Self {
        self.state.lock().unwrap().script.push_back(Expectation {
            cmd: cmd.to_string(),
            response,
        });
        self
    }
}

impl Transport for MockTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        let cmd = String::from_utf8_lossy(data).trim_end().to_string();
        let mut s = self.state.lock().unwrap();
        s.log.push(cmd.clone());
        let exp = s
            .script
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected command {cmd:?} (script exhausted)"));
        assert_eq!(exp.cmd, cmd, "command mismatch");
        if let Some(r) = exp.response {
            s.pending = r;
        }
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        let mut s = self.state.lock().unwrap();
        if s.pending.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "mock: no pending response",
            ));
        }
        let n = buf
            .len()
            .min(s.pending.len())
            .min(self.max_read.unwrap_or(usize::MAX));
        buf[..n].copy_from_slice(&s.pending[..n]);
        s.pending.drain(..n);
        let end = !self.stream && (self.short_transfers || s.pending.is_empty());
        Ok(ReadChunk { n, end })
    }

    fn clear(&mut self) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        s.pending.clear();
        s.clears += 1;
        Ok(())
    }

    fn set_timeout(&mut self, _timeout: Duration) -> io::Result<()> {
        Ok(())
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn describe(&self) -> String {
        "mock".into()
    }
}
