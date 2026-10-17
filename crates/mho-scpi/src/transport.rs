use std::io;
use std::time::Duration;

/// Result of a single [`Transport::read_some`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadChunk {
    /// Number of bytes written into the buffer.
    pub n: usize,
    /// `true` if the transport knows that the device message is complete (USBTMC EOM).
    /// Stream transports (TCP) always report `false` and rely on the `\n` terminator.
    pub end: bool,
}

/// A bidirectional byte pipe to an instrument.
///
/// Implementations are blocking; the async layers above run them on a dedicated thread.
pub trait Transport: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;

    /// Reads at most `buf.len()` bytes of the pending response.
    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk>;

    /// Device clear: aborts pending I/O and empties the instrument's output queue.
    fn clear(&mut self) -> io::Result<()>;

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;

    fn timeout(&self) -> Duration;

    /// Human-readable resource description, e.g. `usbtmc:/dev/usbtmc0`.
    fn describe(&self) -> String;
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        (**self).write_all(data)
    }
    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        (**self).read_some(buf)
    }
    fn clear(&mut self) -> io::Result<()> {
        (**self).clear()
    }
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        (**self).set_timeout(timeout)
    }
    fn timeout(&self) -> Duration {
        (**self).timeout()
    }
    fn describe(&self) -> String {
        (**self).describe()
    }
}
