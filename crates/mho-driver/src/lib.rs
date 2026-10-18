//! Driver for Rigol MHO900-series oscilloscopes (MHO934/954/984), tested on an MHO954.
//!
//! [`Scope`] wraps a [`mho_scpi::Scpi`] session with typed getters/setters; [`SimTransport`]
//! emulates the instrument for development and tests without hardware.

pub mod analysis;
pub mod caps;
mod error;
pub mod generator;
pub mod measure;
pub mod model;
pub mod ops;
pub mod options;
pub mod params;
mod scope;
pub mod sim;
pub mod verify;

pub use analysis::{BodePoint, BodeRequest, LogicCapture, NativeBodeRequest};
pub use caps::{Capabilities, GeneratorCaps};
pub use error::{DriverError, Result};
pub use generator::GeneratorUpdate;
pub use measure::{MEASURE_ITEMS, MeasureItem, find_item};
pub use model::*;
pub use scope::{Connection, Scope};
pub use sim::{SimOptions, SimTransport};
