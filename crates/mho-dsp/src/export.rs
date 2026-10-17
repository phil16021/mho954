//! Waveform export: CSV (time + volts per channel) and NumPy `.npy` (raw ADC codes).

use std::io::{self, Write};
use std::path::Path;

/// One column of a CSV export.
pub struct CsvColumn<'a> {
    pub name: &'a str,
    pub volts: &'a [f64],
}

/// Writes `time,<col>...` rows. `t0` is the time of the first sample.
pub fn write_csv<W: Write>(out: W, t0: f64, dt: f64, columns: &[CsvColumn<'_>]) -> io::Result<()> {
    let mut w = csv::Writer::from_writer(out);
    let mut header = vec!["time_s".to_string()];
    header.extend(columns.iter().map(|c| format!("{}_V", c.name)));
    w.write_record(&header)?;
    let rows = columns.iter().map(|c| c.volts.len()).max().unwrap_or(0);
    let mut record = Vec::with_capacity(columns.len() + 1);
    for i in 0..rows {
        record.clear();
        record.push(format!("{:.12e}", t0 + i as f64 * dt));
        for c in columns {
            record.push(c.volts.get(i).map(|v| format!("{v:.6e}")).unwrap_or_default());
        }
        w.write_record(&record)?;
    }
    w.flush()
}

/// Serialises raw `u16` ADC codes as a 1-D `.npy` array (format version 1.0, little endian).
pub fn npy_bytes(raw: &[u16]) -> io::Result<Vec<u8>> {
    let mut header = format!("{{'descr': '<u2', 'fortran_order': False, 'shape': ({},), }}", raw.len());
    // magic(6) + version(2) + header length(2) + header must be a multiple of 64, ending in '\n'.
    let total = 10 + header.len() + 1;
    header.push_str(&" ".repeat((64 - total % 64) % 64));
    header.push('\n');
    let header_len = u16::try_from(header.len()).map_err(|_| io::Error::other("npy header too long"))?;

    let mut buf = Vec::with_capacity(10 + header.len() + raw.len() * 2);
    buf.extend_from_slice(b"\x93NUMPY\x01\x00");
    buf.extend_from_slice(&header_len.to_le_bytes());
    buf.extend_from_slice(header.as_bytes());
    for v in raw {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    Ok(buf)
}

pub fn write_npy(path: &Path, raw: &[u16]) -> io::Result<()> {
    std::fs::write(path, npy_bytes(raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_layout() {
        let mut out = Vec::new();
        let a = [1.0, 2.0];
        write_csv(&mut out, 0.0, 1e-3, &[CsvColumn { name: "CHAN1", volts: &a }]).unwrap();
        let s = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "time_s,CHAN1_V");
        assert_eq!(lines.len(), 3);
        assert!(lines[2].starts_with("1.000000000000e-3,2.000000e0"));
    }

    #[test]
    fn npy_roundtrip() {
        let raw = [1u16, 32768, 65535];
        let bytes = npy_bytes(&raw).unwrap();
        assert_eq!(&bytes[..6], b"\x93NUMPY");
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        assert_eq!((10 + header_len) % 64, 0);
        let header = std::str::from_utf8(&bytes[10..10 + header_len]).unwrap();
        assert!(header.contains("'shape': (3,)") && header.ends_with('\n'));
        let data: Vec<u16> =
            bytes[10 + header_len..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(data, raw);
    }
}
