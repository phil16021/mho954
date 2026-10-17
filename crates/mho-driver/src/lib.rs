//! Driver for Rigol MHO900-series oscilloscopes (MHO934/954/984), tested on an MHO954.
//!
//! [`Scope`] wraps a [`mho_scpi::Scpi`] session with typed getters/setters; [`SimTransport`]
//! emulates the instrument for development and tests without hardware.

mod error;
pub mod measure;
pub mod model;
pub mod options;
mod scope;
pub mod sim;
pub mod verify;

pub use error::{DriverError, Result};
pub use measure::{MEASURE_ITEMS, MeasureItem, find_item};
pub use model::*;
pub use scope::{Connection, Scope};
pub use sim::{SimOptions, SimTransport};
