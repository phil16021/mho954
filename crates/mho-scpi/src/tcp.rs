//! Raw SCPI socket transport (Rigol listens on TCP port 5555).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use tracing::info;

use crate::transport::{ReadChunk, Transport};

pub const DEFAULT_PORT: u16 = 5555;

pub struct TcpTransport {
    addr: String,
    stream: TcpStream,
    timeout: Duration,
}

/// Adds the default port unless one is given: `host`, `host:port`, `1.2.3.4`, `[::1]`,
/// `[::1]:5555`, `fe80::1` (bare IPv6 without port).
fn with_default_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr.starts_with('[') {
        if addr.contains("]:") { addr.to_string() } else { format!("{addr}:{DEFAULT_PORT}") }
    } else if addr.matches(':').count() > 1 {
        format!("[{addr}]:{DEFAULT_PORT}")
    } else if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{addr}:{DEFAULT_PORT}")
    }
}

fn connect_any(addr: &str) -> io::Result<TcpStream> {
    let addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "address did not resolve");
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(3)) {
            Ok(s) => {
                s.set_nodelay(true)?;
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

impl TcpTransport {
    /// Connects to `host` or `host:port` (port defaults to 5555).
    pub fn connect(addr: &str) -> io::Result<Self> {
        let addr = with_default_port(addr);
        let stream = connect_any(&addr)?;
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

    /// The raw socket has no device clear. After an unanswered query the scope ignores the
    /// session, so the connection is re-established instead.
    fn clear(&mut self) -> io::Result<()> {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        self.stream = connect_any(&self.addr)?;
        self.set_timeout(self.timeout)?;
        info!(addr = %self.addr, "tcp: reconnected");
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Scpi;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    #[test]
    fn default_port() {
        assert_eq!(with_default_port("192.168.1.5"), "192.168.1.5:5555");
        assert_eq!(with_default_port("scope:1234"), "scope:1234");
        assert_eq!(with_default_port("fe80::1"), "[fe80::1]:5555");
        assert_eq!(with_default_port("[::1]"), "[::1]:5555");
        assert_eq!(with_default_port("[::1]:99"), "[::1]:99");
    }

    /// Minimal SCPI socket server: answers `*IDN?` and a block query, ignores unknown queries.
    fn server() -> (String, std::thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let mut connections = 0;
            for stream in listener.incoming().take(2) {
                connections += 1;
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    match line.trim() {
                        "*IDN?" => stream.write_all(b"RIGOL TECHNOLOGIES,MHO954,X,1\n").unwrap(),
                        ":DATA?" => stream.write_all(b"#13abc\n").unwrap(),
                        "QUIT" => return connections,
                        _ => {}
                    }
                    line.clear();
                }
            }
            connections
        });
        (addr, h)
    }

    #[test]
    fn query_block_and_reconnect_after_unanswered_query() {
        let (addr, h) = server();
        let mut s = Scpi::new(Box::new(TcpTransport::connect(&addr).unwrap()));
        s.set_timeout(Duration::from_millis(300)).unwrap();
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
        assert_eq!(s.query_block(":DATA?").unwrap(), b"abc");
        assert!(s.query(":UNKNOWN?").unwrap_err().is_timeout());
        // The session was re-established by the clear.
        assert!(s.query("*IDN?").unwrap().starts_with("RIGOL"));
        s.write("QUIT").unwrap();
        assert_eq!(h.join().unwrap(), 2);
    }
}
