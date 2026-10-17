//! Protocol decoder event tables as returned by `:BUS<n>:DATA?`:
//!
//! ```text
//! PARALLEL
//! Time,Data,
//! -2.47us,0,
//! 551.6ns,1,
//! ```
//!
//! The first line names the protocol, the second holds the column titles; every line ends
//! with a comma. Values are formatted in the bus's number format (HEX/ASCII/DEC/BIN).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct DecodeTable {
    /// e.g. `PARALLEL`, `RS232`, `I2C`, `SPI`, `CAN`, `LIN` (empty if not reported).
    pub protocol: String,
    /// Column titles, e.g. `["Time", "Data"]`.
    pub columns: Vec<String>,
    pub rows: Vec<DecodeRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct DecodeRow {
    /// Event time in seconds relative to the trigger, if the first cell is a time.
    pub time: Option<f64>,
    /// All cells as reported, including the time text.
    pub cells: Vec<String>,
}

fn cells(line: &str) -> Vec<String> {
    let line = line.trim_end_matches(['\r', '\n']);
    let line = line.strip_suffix(',').unwrap_or(line);
    line.split(',').map(|c| c.trim().to_string()).collect()
}

/// Parses the payload of `:BUS<n>:DATA?` (without the IEEE block header).
pub fn parse_decode_table(text: &str) -> DecodeTable {
    let mut lines = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty());
    let mut table = DecodeTable::default();
    let Some(first) = lines.next() else {
        return table;
    };
    let header = if first.contains(',') {
        first
    } else {
        table.protocol = first.trim().to_string();
        match lines.next() {
            Some(h) => h,
            None => return table,
        }
    };
    table.columns = cells(header);
    let time_first = table
        .columns
        .first()
        .is_some_and(|c| c.eq_ignore_ascii_case("time"));
    for line in lines {
        let cells = cells(line);
        let time = if time_first {
            cells.first().and_then(|c| parse_si_value(c))
        } else {
            None
        };
        table.rows.push(DecodeRow { time, cells });
    }
    table
}

/// Parses a number with optional SI prefix and unit as printed by the scope:
/// `-2.47us`, `551.6ns`, `2.50000MHz`, `1.2E-3`, `-12.5 mV`.
pub fn parse_si_value(s: &str) -> Option<f64> {
    let s = s.trim();
    // Longest prefix that is a number (handles exponents like 1E-3 before a unit).
    let end = (1..=s.len())
        .rev()
        .filter(|&i| s.is_char_boundary(i))
        .find(|&i| s[..i].parse::<f64>().is_ok())?;
    let x: f64 = s[..end].parse().ok()?;
    let rest = s[end..].trim_start();
    let mut chars = rest.chars();
    let mult = match (chars.next(), chars.next()) {
        (None, _) => 1.0,
        // A lone prefix letter is a prefix ("5m"), a lone unit letter is a unit ("5s").
        (Some(p), next) => {
            let m = match p {
                'f' => 1e-15,
                'p' => 1e-12,
                'n' => 1e-9,
                'u' | 'µ' | 'μ' => 1e-6,
                'm' => 1e-3,
                'k' | 'K' => 1e3,
                'M' => 1e6,
                'G' => 1e9,
                _ => 1.0,
            };
            if next.is_none()
                && !matches!(p, 'f' | 'p' | 'n' | 'u' | 'µ' | 'μ' | 'm' | 'k' | 'M' | 'G')
            {
                1.0
            } else {
                m
            }
        }
    };
    Some(x * mult)
}

/// CSV export (column titles, then one line per event), as the scope writes it.
pub fn decode_csv(table: &DecodeTable) -> String {
    let quote = |c: &str| {
        if c.contains([',', '"', '\n']) {
            format!("\"{}\"", c.replace('"', "\"\""))
        } else {
            c.to_string()
        }
    };
    let mut out = String::new();
    let row = |cells: &[String]| cells.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",");
    out.push_str(&row(&table.columns));
    out.push('\n');
    for r in &table.rows {
        out.push_str(&row(&r.cells));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUIDE: &str = "PARALLEL\nTime,Data,\n-2.47us,0,\n-2.444us,1,\n-1.448us,0,\n\
                         -446ns,1,\n551.6ns,0,\n1.554us,1,\n";

    #[test]
    fn guide_example() {
        let t = parse_decode_table(GUIDE);
        assert_eq!(t.protocol, "PARALLEL");
        assert_eq!(t.columns, ["Time", "Data"]);
        assert_eq!(t.rows.len(), 6);
        assert_eq!(t.rows[0].cells, ["-2.47us", "0"]);
        assert!((t.rows[0].time.unwrap() + 2.47e-6).abs() < 1e-15);
        assert!((t.rows[4].time.unwrap() - 551.6e-9).abs() < 1e-15);
    }

    #[test]
    fn crlf_and_missing_protocol() {
        let t = parse_decode_table("Time,TX,RX,\r\n1ms,0x41,,\r\n");
        assert_eq!(t.protocol, "");
        assert_eq!(t.columns, ["Time", "TX", "RX"]);
        assert_eq!(t.rows[0].cells, ["1ms", "0x41", ""]);
        assert_eq!(t.rows[0].time, Some(1e-3));
    }

    #[test]
    fn empty_tables() {
        assert_eq!(parse_decode_table(""), DecodeTable::default());
        let t = parse_decode_table("SPI\n");
        assert_eq!(t.protocol, "SPI");
        assert!(t.columns.is_empty() && t.rows.is_empty());
        let t = parse_decode_table("SPI\nTime,MISO,MOSI,\n");
        assert_eq!(t.columns.len(), 3);
        assert!(t.rows.is_empty());
    }

    #[test]
    fn si_values() {
        assert_eq!(parse_si_value("2.50000MHz"), Some(2.5e6));
        assert_eq!(parse_si_value("1.5s"), Some(1.5));
        assert_eq!(parse_si_value("5m"), Some(5e-3));
        assert_eq!(parse_si_value("1.2E-3"), Some(1.2e-3));
        assert_eq!(parse_si_value("-12.5 mV"), Some(-12.5e-3));
        assert_eq!(parse_si_value("3 µs"), Some(3e-6));
        assert_eq!(parse_si_value("-40dB"), Some(-40.0));
        assert_eq!(parse_si_value("abc"), None);
    }

    #[test]
    fn csv_roundtrip_quoting() {
        let mut t = parse_decode_table("RS232\nTime,TX,\n1us,41,\n");
        assert_eq!(decode_csv(&t), "Time,TX\n1us,41\n");
        t.rows[0].cells[1] = "a,\"b\"".into();
        assert_eq!(decode_csv(&t), "Time,TX\n1us,\"a,\"\"b\"\"\"\n");
    }
}
