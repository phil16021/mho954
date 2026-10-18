//! Driver for Rigol MHO900-series oscilloscopes (MHO934/954/984), tested on an MHO954.
//!
//! [`Scope`] wraps a [`mho_scpi::Scpi`] session with typed getters/setters; [`SimTransport`]
//! emulates the instrument for development and tests without hardware.
//!
//! * [`caps`]: what this scope can do (bandwidth after upgrades, memory limits, generator,
//!   licensed protocols), derived from `*IDN?`, the options and `:SYST:MOD?`.
//! * [`params`] / [`ops`]: tables of settings groups, readouts and one-shot commands, from
//!   which validation, all frontends and the verification candidates are derived. Reading is
//!   gated on queries confirmed on the instrument (`docs/verified-commands.json`, [`verify`]).
//! * [`generator`]: function generator limits and typed updates; [`analysis`]: software and
//!   scope-native Bode plots, logic-analyzer capture.

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
