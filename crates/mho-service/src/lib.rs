//! Service layer between the instrument and its frontends.
//!
//! * [`Device`] owns the [`mho_driver::Scope`] on a dedicated thread and serialises access.
//! * [`LocalBackend`] implements [`ScopeApi`] on top of it and runs the live acquisition loop.
//! * [`RemoteBackend`] implements the same trait against a running `mho954 serve` daemon.
//! * The live hub publishes frames, measurements, errors, progress of long operations and the
//!   state of every Bode sweep ([`LiveEvent`]); the WebSocket forwards them to all viewers.

mod api;
mod device;
pub mod live;
mod local;
mod remote;

pub use api::*;
pub use device::{Device, Opener};
pub use live::{BodeLive, Frame, LiveConfig, LiveEvent, MeasureSnapshot, MeasureSpec, Progress};
pub use local::LocalBackend;
pub use remote::RemoteBackend;
