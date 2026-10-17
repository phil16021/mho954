//! Signal processing on captured waveforms: amplitude/timing measurements, FFT,
//! min/max decimation for display and file export.

pub mod decimate;
pub mod export;
pub mod fft;
pub mod measure;

pub use decimate::{MinMax, minmax_decimate};
pub use fft::{Spectrum, SpectrumPeak, Window, spectrum};
pub use measure::{Measurements, analyze};
