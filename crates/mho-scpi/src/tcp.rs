//! Raw SCPI socket transport (Rigol listens on TCP port 5555).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::transport::{ReadChunk, Transport};

pub const DEFAULT_PORT: u16 = 5555;

pub struct TcpTransport {
    addr: String,
    stream: TcpStream,
    timeout: Duration,
}

impl TcpTransport {
    /// Connects to `host` or `host:port` (port defaults to 5555).
    pub fn connect(addr: &str) -> io::Result<Self> {
        let addr = if addr.contains(':') { addr.to_string() } else { format!("{addr}:{DEFAULT_PORT}") };
        let sock = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "address did not resolve"))?;
        let stream = TcpStream::connect_timeout(&sock, Duration::from_secs(3))?;
        stream.set_nodelay(true)?;
        let mut t = TcpTransport { addr, stream, timeout: Duration::from_secs(2) };
        t.set_timeout(t.timeout)?;
        Ok(t)
    }
}

impl Transport for TcpTransport {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream.write_all(data)
    }

    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        let n = self.stream.read(buf)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }
        Ok(ReadChunk { n, end: false })
    }

    /// The socket protocol has no device clear; drain whatever is still in flight.
    fn clear(&mut self) -> io::Result<()> {
        self.stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut sink = [0u8; 65536];
        while let Ok(n) = self.stream.read(&mut sink) {
            if n == 0 {
                break;
            }
        }
        self.stream.set_read_timeout(Some(self.timeout))
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.set_write_timeout(Some(timeout))?;
        self.timeout = timeout;
        Ok(())
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn describe(&self) -> String {
        format!("tcp:{}", self.addr)
    }
}
