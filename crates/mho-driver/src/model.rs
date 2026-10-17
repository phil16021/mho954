//! Data types exchanged with the scope, shared by the REST API, MCP tools and GUI.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{DriverError, Result};

pub const CHANNELS: u8 = 4;

/// Defines a fieldless enum with its SCPI token and accepted aliases.
macro_rules! scpi_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident => $tok:literal $(| $alias:literal)*),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
        pub enum $name { $($(#[$vm])* $var),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];

            pub fn scpi(self) -> &'static str {
                match self { $($name::$var => $tok),+ }
            }

            pub fn from_scpi(s: &str) -> Result<Self> {
                let u = s.trim().to_ascii_uppercase();
                $(if u == $tok $(|| u == $alias)* { return Ok($name::$var); })+
                Err(DriverError::Invalid(format!(concat!("unknown ", stringify!($name), " {:?}"), s)))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.scpi())
            }
        }

        impl std::str::FromStr for $name {
            type Err = DriverError;
            fn from_str(s: &str) -> Result<Self> { Self::from_scpi(s) }
        }
    };
}

scpi_enum!(Coupling { DC => "DC", AC => "AC", GND => "GND" });

scpi_enum!(
    /// Input impedance: 1 MΩ or 50 Ω.
    Impedance { OneMeg => "OMEG" | "1M" | "1MEG", Fifty => "FIFT" | "50" | "FIFTY" }
);

scpi_enum!(TriggerSweep { Auto => "AUTO", Normal => "NORM" | "NORMAL", Single => "SING" | "SINGLE" });

scpi_enum!(Slope { Positive => "POS" | "RISING" | "RISE", Negative => "NEG" | "FALLING" | "FALL", Either => "RFAL" | "EITHER" | "BOTH" });

scpi_enum!(AcquireType { Normal => "NORM" | "NORMAL", Peak => "PEAK", Average => "AVER" | "AVERAGES" | "AVERAGE", HighRes => "HRES" | "HRESOLUTION" });

scpi_enum!(
    /// `:WAVeform:MODE`: `NORM` = screen data (1000 pts), `RAW` = acquisition memory (needs STOP).
    WaveMode { Normal => "NORM" | "NORMAL", Raw => "RAW", Max => "MAX" | "MAXIMUM" }
);

scpi_enum!(
    /// Front-panel style actions.
    Action { Run => "RUN", Stop => "STOP", Single => "SING" | "SINGLE", Autoscale => "AUT" | "AUTO" | "AUTOSCALE", Clear => "CLE" | "CLEAR", Force => "TFOR" | "FORCE" }
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Idn {
    pub manufacturer: String,
    pub model: String,
    pub serial: String,
    pub firmware: String,
}

impl Idn {
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.trim().splitn(4, ',').map(str::trim).collect();
        if parts.len() < 4 {
            return Err(DriverError::Invalid(format!(
                "unexpected *IDN? response {s:?}"
            )));
        }
        Ok(Idn {
            manufacturer: parts[0].into(),
            model: parts[1].into(),
            serial: parts[2].into(),
            firmware: parts[3].into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ChannelState {
    pub channel: u8,
    pub display: bool,
    /// Volts per division.
    pub scale: f64,
    /// Vertical offset in volts.
    pub offset: f64,
    pub coupling: Coupling,
    /// Bandwidth limit, e.g. `OFF`, `20M`.
    pub bw_limit: String,
    /// Probe attenuation ratio.
    pub probe: f64,
    pub impedance: Impedance,
    pub invert: bool,
    /// Fine (vernier) scale adjustment.
    pub vernier: bool,
    pub unit: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ChannelUpdate {
    pub display: Option<bool>,
    /// Volts per division.
    pub scale: Option<f64>,
    /// Vertical offset in volts.
    pub offset: Option<f64>,
    pub coupling: Option<Coupling>,
    /// `OFF`, `20M`, `250M`.
    pub bw_limit: Option<String>,
    pub probe: Option<f64>,
    pub impedance: Option<Impedance>,
    pub invert: Option<bool>,
    pub vernier: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct TimebaseState {
    /// Seconds per division.
    pub scale: f64,
    /// Horizontal offset (trigger position) in seconds.
    pub offset: f64,
    /// `MAIN`, `XY`, `ROLL`.
    pub mode: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct TimebaseUpdate {
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    /// `MAIN`, `XY`, `ROLL`.
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct TriggerState {
    /// Trigger type, e.g. `EDGE`, `PULS`, `SLOP`.
    pub mode: String,
    pub sweep: TriggerSweep,
    /// `TD`, `WAIT`, `RUN`, `AUTO`, `STOP`.
    pub status: String,
    pub coupling: String,
    pub holdoff: f64,
    pub edge_source: String,
    pub edge_slope: Slope,
    pub edge_level: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct TriggerUpdate {
    /// Trigger type; only `EDGE` has dedicated fields here, others via raw SCPI.
    pub mode: Option<String>,
    pub sweep: Option<TriggerSweep>,
    pub coupling: Option<String>,
    pub holdoff: Option<f64>,
    /// `CHAN1`..`CHAN4`, `EXT`, `ACL`, `D0`..`D15`.
    pub edge_source: Option<String>,
    pub edge_slope: Option<Slope>,
    pub edge_level: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AcquireState {
    /// Samples per second.
    pub sample_rate: f64,
    /// Memory depth in points (`None` if the scope reported something unparsable).
    pub memory_depth: Option<f64>,
    pub acq_type: AcquireType,
    pub averages: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct AcquireUpdate {
    /// `AUTO`, `1k`, `10k`, `100k`, `1M`, `10M`, `25M`, `50M`, `100M` (125M…500M with RLU-05).
    pub memory_depth: Option<String>,
    pub acq_type: Option<AcquireType>,
    /// Power of two, used with `acq_type = Average`.
    pub averages: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ScopeStatus {
    pub idn: Idn,
    pub trigger_status: String,
    pub channels: Vec<ChannelState>,
    pub timebase: TimebaseState,
    pub trigger: TriggerState,
    pub acquire: AcquireState,
}

/// `:WAVeform:PREamble?` — scaling information for waveform data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Preamble {
    /// 0 = BYTE, 1 = WORD, 2 = ASCII.
    pub format: u8,
    /// 0 = NORM, 1 = MAX, 2 = RAW.
    pub kind: u8,
    pub points: usize,
    pub count: u32,
    pub x_increment: f64,
    pub x_origin: f64,
    pub x_reference: f64,
    pub y_increment: f64,
    pub y_origin: f64,
    pub y_reference: f64,
}

impl Preamble {
    pub fn parse(s: &str) -> Result<Self> {
        let f: Vec<f64> = s
            .trim()
            .split(',')
            .map(|x| x.trim().parse::<f64>())
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| DriverError::Invalid(format!("preamble {s:?}: {e}")))?;
        if f.len() < 10 {
            return Err(DriverError::Invalid(format!(
                "preamble has {} fields: {s:?}",
                f.len()
            )));
        }
        Ok(Preamble {
            format: f[0] as u8,
            kind: f[1] as u8,
            points: f[2] as usize,
            count: f[3] as u32,
            x_increment: f[4],
            x_origin: f[5],
            x_reference: f[6],
            y_increment: f[7],
            y_origin: f[8],
            y_reference: f[9],
        })
    }

    pub fn to_scpi(&self) -> String {
        format!(
            "{},{},{},{},{:e},{:e},{},{:e},{},{}",
            self.format,
            self.kind,
            self.points,
            self.count,
            self.x_increment,
            self.x_origin,
            self.x_reference,
            self.y_increment,
            self.y_origin,
            self.y_reference
        )
    }

    /// Converts a raw ADC code to volts: `(raw - yorigin - yreference) * yincrement`.
    #[inline]
    pub fn volts(&self, raw: u16) -> f64 {
        (raw as f64 - self.y_origin - self.y_reference) * self.y_increment
    }

    /// Time of sample `i` relative to the trigger.
    #[inline]
    pub fn time(&self, i: usize) -> f64 {
        (i as f64 - self.x_reference) * self.x_increment + self.x_origin
    }
}

/// A captured waveform: raw codes plus the preamble needed to scale them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Waveform {
    pub source: String,
    pub preamble: Preamble,
    pub raw: Vec<u16>,
}

impl Waveform {
    pub fn volts(&self) -> Vec<f64> {
        self.raw.iter().map(|&r| self.preamble.volts(r)).collect()
    }

    pub fn times(&self) -> Vec<f64> {
        (0..self.raw.len()).map(|i| self.preamble.time(i)).collect()
    }

    pub fn analyze(&self) -> mho_dsp::Measurements {
        mho_dsp::analyze(&self.volts(), self.preamble.x_increment)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct Measurement {
    pub item: String,
    pub source: String,
    /// `None` when the scope reports the value as invalid (9.9E37).
    pub value: Option<f64>,
    pub unit: String,
}

scpi_enum!(
    /// LAN address assignment.
    IpMode { Dhcp => "DHCP", AutoIp => "AUTOIP" | "AUTO", Static => "STATIC" | "MANUAL" | "MAN" }
);

/// LAN (RJ45) configuration and state (`:LAN` subsystem).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct NetworkState {
    /// `UNLINK`, `CONNECTED`, `INIT`, `IPCONFLICT`, `BUSY`, `CONFIGURED`, `DHCPFAILED`,
    /// `INVALIDIP`, `IPLOSE`.
    pub status: String,
    pub dhcp: bool,
    pub auto_ip: bool,
    /// Static IP configuration enabled.
    pub manual: bool,
    pub ip: String,
    pub mask: String,
    pub gateway: String,
    pub dns: String,
    pub mac: String,
    pub dhcp_server: String,
    pub mdns: bool,
    pub hostname: String,
    pub description: String,
    /// VISA resource string for LXI (VXI-11).
    pub visa: String,
}

/// LAN changes; applied together with `:LAN:APPLy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct NetworkUpdate {
    /// `Dhcp`, `AutoIp` or `Static` (ip/mask/gateway/dns only take effect with `Static`).
    pub mode: Option<IpMode>,
    pub ip: Option<String>,
    pub mask: Option<String>,
    pub gateway: Option<String>,
    pub dns: Option<String>,
    pub mdns: Option<bool>,
    pub hostname: Option<String>,
    pub description: Option<String>,
}

/// Validates an IPv4 address as the scope accepts it: first octet 0–223 except 127
/// (`mask = true`: plain dotted quad with contiguous one-bits).
pub fn validate_ipv4(s: &str, mask: bool) -> Result<String> {
    let invalid = || {
        DriverError::Invalid(format!(
            "invalid {} {s:?}",
            if mask { "subnet mask" } else { "IPv4 address" }
        ))
    };
    let octets: Vec<u8> = s
        .trim()
        .split('.')
        .map(|p| p.parse::<u8>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| invalid())?;
    if octets.len() != 4 {
        return Err(invalid());
    }
    if mask {
        let v = u32::from_be_bytes([octets[0], octets[1], octets[2], octets[3]]);
        if v.leading_ones() + v.trailing_zeros() != 32 {
            return Err(invalid());
        }
    } else if octets[0] > 223 || octets[0] == 127 {
        return Err(invalid());
    }
    Ok(octets
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct OptionInfo {
    pub name: String,
    pub description: String,
    pub active: bool,
}

/// Normalises a waveform/measurement source (`1`, `ch1`, `CHANnel1` → `CHAN1`, `math1` → `MATH1`,
/// `d3` → `D3`).
pub fn normalize_source(s: &str) -> Result<String> {
    let u = s.trim().to_ascii_uppercase();
    let invalid = || {
        DriverError::Invalid(format!(
            "unknown source {s:?} (use CHAN1..4, MATH1..4, D0..D15)"
        ))
    };
    let digits_after = |prefixes: &[&str]| -> Option<u8> {
        prefixes
            .iter()
            .find_map(|p| u.strip_prefix(p).and_then(|rest| rest.parse::<u8>().ok()))
    };
    if let Ok(n) = u.parse::<u8>() {
        return (1..=CHANNELS)
            .contains(&n)
            .then(|| format!("CHAN{n}"))
            .ok_or_else(invalid);
    }
    if let Some(n) = digits_after(&["CHANNEL", "CHAN", "CH", "C"]) {
        return (1..=CHANNELS)
            .contains(&n)
            .then(|| format!("CHAN{n}"))
            .ok_or_else(invalid);
    }
    if let Some(n) = digits_after(&["MATH", "M"]) {
        return (1..=4)
            .contains(&n)
            .then(|| format!("MATH{n}"))
            .ok_or_else(invalid);
    }
    if let Some(n) = digits_after(&["D"]) {
        return (n <= 15).then(|| format!("D{n}")).ok_or_else(invalid);
    }
    Err(invalid())
}

/// Channel number (1..=4) from a normalised `CHANn` source.
pub fn channel_of(source: &str) -> Option<u8> {
    source.strip_prefix("CHAN").and_then(|n| n.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_preamble() {
        // Captured from the MHO954 (FW 00.01.00), CH1 at 50 mV/div, 2 µs/div.
        let p = Preamble::parse("1,0,1000,1,2.000000E-8,-1.000000E-5,0.000000,6.6667E-06,0,32768")
            .unwrap();
        assert_eq!(p.points, 1000);
        assert_eq!(p.format, 1);
        assert_eq!(p.volts(32768), 0.0);
        assert!((p.volts(32768 + 7500) - 0.05).abs() < 1e-6);
        assert!((p.time(0) + 10e-6).abs() < 1e-15);
        assert!((p.time(1000) - 10e-6).abs() < 1e-12);
    }

    #[test]
    fn idn_parse() {
        let idn = Idn::parse("RIGOL TECHNOLOGIES,MHO954,MHO9X000000000,00.01.00\n").unwrap();
        assert_eq!(idn.model, "MHO954");
        assert_eq!(idn.firmware, "00.01.00");
    }

    #[test]
    fn sources() {
        assert_eq!(normalize_source("1").unwrap(), "CHAN1");
        assert_eq!(normalize_source("ch4").unwrap(), "CHAN4");
        assert_eq!(normalize_source("CHANnel2").unwrap(), "CHAN2");
        assert_eq!(normalize_source("math1").unwrap(), "MATH1");
        assert_eq!(normalize_source("d15").unwrap(), "D15");
        assert!(normalize_source("CHAN5").is_err());
        assert!(normalize_source("foo").is_err());
    }

    #[test]
    fn ipv4_validation() {
        assert_eq!(
            validate_ipv4(" 192.168.1.20 ", false).unwrap(),
            "192.168.1.20"
        );
        assert!(validate_ipv4("127.0.0.1", false).is_err());
        assert!(validate_ipv4("224.0.0.1", false).is_err());
        assert!(validate_ipv4("10.0.0", false).is_err());
        assert!(validate_ipv4("10.0.0.1;*RST", false).is_err());
        assert_eq!(
            validate_ipv4("255.255.255.0", true).unwrap(),
            "255.255.255.0"
        );
        assert!(validate_ipv4("255.0.255.0", true).is_err());
    }

    #[test]
    fn enum_tokens() {
        assert_eq!(Impedance::from_scpi("OMEG").unwrap(), Impedance::OneMeg);
        assert_eq!(Impedance::from_scpi("50").unwrap(), Impedance::Fifty);
        assert_eq!(Slope::from_scpi("rising").unwrap(), Slope::Positive);
        assert_eq!(TriggerSweep::Single.scpi(), "SING");
        assert!(Coupling::from_scpi("XX").is_err());
    }
}
