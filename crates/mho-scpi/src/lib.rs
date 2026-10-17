//! SCPI transport layer for Rigol oscilloscopes.
//!
//! [`Transport`] abstracts the byte pipe (Linux `usbtmc` kernel driver, raw TCP socket on
//! port 5555, or a scripted mock), [`Scpi`] adds message framing on top: line responses,
//! IEEE 488.2 definite-length blocks and automatic recovery after timeouts.

mod block;
mod error;
pub mod mock;
mod scpi;
mod tcp;
mod transport;
mod usbtmc;

pub use block::{BlockHeader, parse_block_header};
pub use error::{Result, ScpiError};
pub use scpi::Scpi;
pub use tcp::TcpTransport;
pub use transport::{ReadChunk, Transport};
pub use usbtmc::{UsbTmcTransport, find_usbtmc_devices};
