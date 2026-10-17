//! Signal processing on captured waveforms: amplitude/timing measurements, FFT,
//! min/max decimation for display and file export.

pub mod decimate;
pub mod export;
pub mod fft;
pub mod measure;

pub use decimate::{MinMax, minmax_decimate};
pub use fft::{Spectrum, SpectrumPeak, Window, spectrum};
pub use measure::{Measurements, analyze};

/// Formats `v` with an SI prefix, e.g. `format_si(1234.0, "Hz")` → `1.234 kHz`.
pub fn format_si(v: f64, unit: &str) -> String {
    if !v.is_finite() {
        return format!("--- {unit}");
    }
    if v == 0.0 {
        return format!("0 {unit}");
    }
    const PREFIXES: [(f64, &str); 9] =
        [(1e9, "G"), (1e6, "M"), (1e3, "k"), (1.0, ""), (1e-3, "m"), (1e-6, "µ"), (1e-9, "n"), (1e-12, "p"), (1e-15, "f")];
    let a = v.abs();
    let (scale, prefix) = PREFIXES.iter().find(|(s, _)| a >= *s * 0.9995).copied().unwrap_or((1e-15, "f"));
    let x = v / scale;
    let digits = if x.abs() >= 100.0 { 1 } else if x.abs() >= 10.0 { 2 } else { 3 };
    format!("{x:.digits$} {prefix}{unit}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn si() {
        assert_eq!(super::format_si(1234.0, "Hz"), "1.234 kHz");
        assert_eq!(super::format_si(0.05, "V"), "50.00 mV");
        assert_eq!(super::format_si(-2e-6, "s"), "-2.000 µs");
        assert_eq!(super::format_si(0.0, "V"), "0 V");
        assert_eq!(super::format_si(999.99, "V"), "1.000 kV");
    }
}
