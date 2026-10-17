//! Table-driven access to the scope's setting groups (trigger types, decoders, math, cursor,
//! display, …). Each group is a list of parameters with their SCPI node and the value range
//! from the MHO900 programming guide; reading, writing, validation, the REST/MCP/CLI
//! surfaces and the hardware verification candidates are all derived from the tables.
//!
//! Reading a group sends queries. Because the MHO954 hangs after a few unknown queries, a
//! group is only read if all its queries were confirmed on the instrument
//! (`docs/verified-commands.json`, produced by `mho954 verify`) — unless explicitly allowed.
//! Writing only sends commands, whose errors end up in the error queue.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{DriverError, Result};

/// A parameter value as exchanged over the API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(u64),
    Real(f64),
    Text(String),
}

pub type ParamMap = BTreeMap<String, ParamValue>;

#[derive(Debug, Clone, Copy)]
pub enum ParamKind {
    /// Real number; the scope enforces dynamic limits (e.g. levels depend on V/div).
    Real {
        min: f64,
        max: f64,
        unit: &'static str,
    },
    Int {
        min: u64,
        max: u64,
    },
    /// SCPI long forms, e.g. `POSitive`; the short form is the leading upper-case part.
    Enum(&'static [&'static str]),
    Bool,
    /// Analog channel (`CHAN1`..`CHAN4`), optionally also digital `D0`..`D15`.
    Source {
        digital: bool,
    },
    /// Free text (labels), restricted to safe characters.
    Text {
        max_len: usize,
    },
    /// Comma-separated pattern of up to `count` symbols from `symbols`.
    Pattern {
        symbols: &'static str,
        count: usize,
    },
    /// Level of the source selected by parameter `source_param`:
    /// written as `node <src>,<level>`, read as `node? <src>`.
    SourceLevel {
        source_param: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    /// SCPI node (short form); `{n}` is replaced by the group instance.
    pub node: &'static str,
    pub kind: ParamKind,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Requires {
    Nothing,
    /// Logic analyzer hardware (`:SYST:MOD?` first field) and probe.
    LogicAnalyzer,
    /// Function generator hardware (`:SYST:MOD?` second field, AFG option).
    FunctionGenerator,
}

#[derive(Debug, Clone, Copy)]
pub struct Group {
    /// e.g. `trigger.pulse`.
    pub id: &'static str,
    pub title: &'static str,
    /// Command that activates this group (e.g. `:TRIG:MODE PULS`), if any.
    pub select: Option<&'static str>,
    /// Instance range for nodes containing `{n}` (e.g. bus 1..4).
    pub instances: Option<(u8, u8)>,
    pub requires: Requires,
    pub params: &'static [ParamSpec],
}

// ---- tables --------------------------------------------------------------------------------

const fn real(
    name: &'static str,
    node: &'static str,
    min: f64,
    max: f64,
    unit: &'static str,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        node,
        kind: ParamKind::Real { min, max, unit },
        help,
    }
}
const fn int(
    name: &'static str,
    node: &'static str,
    min: u64,
    max: u64,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        node,
        kind: ParamKind::Int { min, max },
        help,
    }
}
const fn en(
    name: &'static str,
    node: &'static str,
    values: &'static [&'static str],
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        node,
        kind: ParamKind::Enum(values),
        help,
    }
}
const fn boolean(name: &'static str, node: &'static str, help: &'static str) -> ParamSpec {
    ParamSpec {
        name,
        node,
        kind: ParamKind::Bool,
        help,
    }
}
const fn src(
    name: &'static str,
    node: &'static str,
    digital: bool,
    help: &'static str,
) -> ParamSpec {
    ParamSpec {
        name,
        node,
        kind: ParamKind::Source { digital },
        help,
    }
}
const fn level(name: &'static str, node: &'static str, help: &'static str) -> ParamSpec {
    real(name, node, -1000.0, 1000.0, "V", help)
}
const fn time(name: &'static str, node: &'static str, min: f64, help: &'static str) -> ParamSpec {
    real(name, node, min, 10.0, "s", help)
}

const POL: &[&str] = &["POSitive", "NEGative"];
const WHEN3: &[&str] = &["GREater", "LESS", "GLESs"];

pub const GROUPS: &[Group] = &[
    Group {
        id: "trigger.pulse",
        title: "Pulse-width trigger",
        select: Some(":TRIG:MODE PULS"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:PULS:SOUR", true, "trigger source"),
            en("polarity", ":TRIG:PULS:POL", POL, "pulse polarity"),
            en("when", ":TRIG:PULS:WHEN", WHEN3, "width condition"),
            time(
                "lower",
                ":TRIG:PULS:LWID",
                1e-9,
                "lower width limit (GREater/GLESs)",
            ),
            time(
                "upper",
                ":TRIG:PULS:UWID",
                1e-9,
                "upper width limit (LESS/GLESs)",
            ),
            level("level", ":TRIG:PULS:LEV", "trigger level"),
        ],
    },
    Group {
        id: "trigger.slope",
        title: "Slope (rise/fall time) trigger",
        select: Some(":TRIG:MODE SLOP"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:SLOP:SOUR", false, "trigger source"),
            en(
                "polarity",
                ":TRIG:SLOP:POL",
                POL,
                "rising (POS) or falling (NEG) edge",
            ),
            en("when", ":TRIG:SLOP:WHEN", WHEN3, "time condition"),
            time("lower", ":TRIG:SLOP:TLOW", 1e-9, "lower time limit"),
            time("upper", ":TRIG:SLOP:TUPP", 1e-9, "upper time limit"),
            en(
                "window",
                ":TRIG:SLOP:WIND",
                &["TA", "TB", "TAB"],
                "level window",
            ),
            level("level_a", ":TRIG:SLOP:ALEV", "upper level"),
            level("level_b", ":TRIG:SLOP:BLEV", "lower level"),
        ],
    },
    Group {
        id: "trigger.video",
        title: "Video trigger",
        select: Some(":TRIG:MODE VID"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:VID:SOUR", false, "trigger source"),
            en("polarity", ":TRIG:VID:POL", POL, "sync polarity"),
            en(
                "mode",
                ":TRIG:VID:MODE",
                &["ODDField", "EVENfield", "LINE", "ALINes"],
                "sync on",
            ),
            int("line", ":TRIG:VID:LINE", 1, 1125, "line number (MODE LINE)"),
            en(
                "standard",
                ":TRIG:VID:STAN",
                &[
                    "PALSecam", "NTSC", "480P", "576P", "720P60", "720P50", "720P30", "720P25",
                    "720P24", "1080P60", "1080P50", "1080P30", "1080P25", "1080P24", "1080I60",
                    "1080I50",
                ],
                "video standard",
            ),
            level("level", ":TRIG:VID:LEV", "trigger level"),
        ],
    },
    Group {
        id: "trigger.pattern",
        title: "Pattern trigger",
        select: Some(":TRIG:MODE PATT"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            ParamSpec {
                name: "pattern",
                node: ":TRIG:PATT:PATT",
                kind: ParamKind::Pattern {
                    symbols: "HLXRF",
                    count: 24,
                },
                help: "states of CH1..CH4, D0..D15 (24 positions on the MHO954): H, L, X, R/F (one edge)",
            },
            src(
                "source",
                ":TRIG:PATT:SOUR",
                true,
                "source whose level is set",
            ),
            ParamSpec {
                name: "level",
                node: ":TRIG:PATT:LEV",
                kind: ParamKind::SourceLevel {
                    source_param: "source",
                },
                help: "threshold of the selected source",
            },
        ],
    },
    Group {
        id: "trigger.duration",
        title: "Duration trigger",
        select: Some(":TRIG:MODE DUR"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src(
                "source",
                ":TRIG:DUR:SOUR",
                true,
                "source whose level is set",
            ),
            ParamSpec {
                name: "pattern",
                node: ":TRIG:DUR:TYPE",
                kind: ParamKind::Pattern {
                    symbols: "HLX",
                    count: 4,
                },
                help: "CH1..CH4 states: H, L, X",
            },
            en(
                "when",
                ":TRIG:DUR:WHEN",
                &["GREater", "LESS", "GLESs", "UNGLess"],
                "duration condition",
            ),
            time("lower", ":TRIG:DUR:TLOW", 1e-9, "lower time limit"),
            time("upper", ":TRIG:DUR:TUPP", 1e-9, "upper time limit"),
            ParamSpec {
                name: "level",
                node: ":TRIG:DUR:LEV",
                kind: ParamKind::SourceLevel {
                    source_param: "source",
                },
                help: "threshold of the selected source",
            },
        ],
    },
    Group {
        id: "trigger.timeout",
        title: "Timeout trigger",
        select: Some(":TRIG:MODE TIM"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:TIM:SOUR", true, "trigger source"),
            en(
                "slope",
                ":TRIG:TIM:SLOP",
                &["POSitive", "NEGative", "RFALl"],
                "edge that starts the timer",
            ),
            time("time", ":TRIG:TIM:TIME", 1e-9, "timeout"),
            level("level", ":TRIG:TIM:LEV", "trigger level"),
        ],
    },
    Group {
        id: "trigger.runt",
        title: "Runt trigger",
        select: Some(":TRIG:MODE RUNT"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:RUNT:SOUR", false, "trigger source"),
            en("polarity", ":TRIG:RUNT:POL", POL, "runt polarity"),
            en(
                "when",
                ":TRIG:RUNT:WHEN",
                &["NONE", "GREater", "LESS", "GLESs"],
                "width condition",
            ),
            time("lower", ":TRIG:RUNT:WLOW", 1e-9, "lower width limit"),
            time("upper", ":TRIG:RUNT:WUPP", 1e-9, "upper width limit"),
            level("level_a", ":TRIG:RUNT:ALEV", "upper level"),
            level("level_b", ":TRIG:RUNT:BLEV", "lower level"),
        ],
    },
    Group {
        id: "trigger.window",
        title: "Window trigger",
        select: Some(":TRIG:MODE WIND"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:WIND:SOUR", false, "trigger source"),
            en(
                "slope",
                ":TRIG:WIND:SLOP",
                &["POSitive", "NEGative", "RFALl"],
                "window edge",
            ),
            en(
                "position",
                ":TRIG:WIND:POS",
                &["EXIT", "ENTer", "TIME"],
                "trigger on exit, entry or time inside",
            ),
            time(
                "time",
                ":TRIG:WIND:TIME",
                1e-9,
                "time inside the window (POSition TIME)",
            ),
            level("level_a", ":TRIG:WIND:ALEV", "upper level"),
            level("level_b", ":TRIG:WIND:BLEV", "lower level"),
        ],
    },
    Group {
        id: "trigger.delay",
        title: "Delay trigger (A → B)",
        select: Some(":TRIG:MODE DEL"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source_a", ":TRIG:DEL:SA", true, "source A"),
            en("slope_a", ":TRIG:DEL:ASL", POL, "edge of source A"),
            src("source_b", ":TRIG:DEL:SB", true, "source B"),
            en("slope_b", ":TRIG:DEL:BSL", POL, "edge of source B"),
            en(
                "type",
                ":TRIG:DEL:TYPE",
                &["GREater", "LESS", "GLESs", "GOUT"],
                "delay condition",
            ),
            time("lower", ":TRIG:DEL:TLOW", 1e-9, "lower delay limit"),
            time("upper", ":TRIG:DEL:TUPP", 1e-9, "upper delay limit"),
            level("level_a", ":TRIG:DEL:ALEV", "threshold of source A"),
            level("level_b", ":TRIG:DEL:BLEV", "threshold of source B"),
        ],
    },
    Group {
        id: "trigger.setup_hold",
        title: "Setup/hold trigger",
        select: Some(":TRIG:MODE SET"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("data", ":TRIG:SHOL:DSRC", true, "data source"),
            src("clock", ":TRIG:SHOL:CSRC", true, "clock source"),
            en("slope", ":TRIG:SHOL:SLOP", POL, "clock edge"),
            en("pattern", ":TRIG:SHOL:PATT", &["H", "L"], "data level"),
            en(
                "type",
                ":TRIG:SHOL:TYPE",
                &["SETup", "HOLD", "SETHold"],
                "violation type",
            ),
            time("setup_time", ":TRIG:SHOL:STIM", 1e-9, "setup time"),
            time("hold_time", ":TRIG:SHOL:HTIM", 1e-9, "hold time"),
            level("data_level", ":TRIG:SHOL:DLEV", "data threshold"),
            level("clock_level", ":TRIG:SHOL:CLEV", "clock threshold"),
        ],
    },
    Group {
        id: "trigger.nth_edge",
        title: "Nth-edge trigger",
        select: Some(":TRIG:MODE NEDG"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:NEDG:SOUR", true, "trigger source"),
            en("slope", ":TRIG:NEDG:SLOP", POL, "edge"),
            time(
                "idle",
                ":TRIG:NEDG:IDLE",
                16e-9,
                "idle time before counting",
            ),
            int(
                "edge",
                ":TRIG:NEDG:EDGE",
                1,
                65535,
                "trigger on the Nth edge",
            ),
            level("level", ":TRIG:NEDG:LEV", "trigger level"),
        ],
    },
    Group {
        id: "trigger.rs232",
        title: "RS232/UART trigger",
        select: Some(":TRIG:MODE RS232"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:RS232:SOUR", true, "data line"),
            level("level", ":TRIG:RS232:LEV", "threshold"),
            en("polarity", ":TRIG:RS232:POL", POL, "idle polarity"),
            en(
                "when",
                ":TRIG:RS232:WHEN",
                &["STARt", "ERRor", "CERRor", "DATA"],
                "trigger condition",
            ),
            int("data", ":TRIG:RS232:DATA", 0, 255, "data value (WHEN DATA)"),
            int("baud", ":TRIG:RS232:BAUD", 1, 20_000_000, "baud rate"),
            en(
                "width",
                ":TRIG:RS232:WIDT",
                &["5", "6", "7", "8"],
                "data bits",
            ),
            en("stop", ":TRIG:RS232:STOP", &["1", "1.5", "2"], "stop bits"),
            en(
                "parity",
                ":TRIG:RS232:PAR",
                &["EVEN", "ODD", "NONE"],
                "parity",
            ),
        ],
    },
    Group {
        id: "trigger.i2c",
        title: "I2C trigger",
        select: Some(":TRIG:MODE IIC"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("scl", ":TRIG:IIC:SCL", true, "clock line"),
            level("scl_level", ":TRIG:IIC:CLEV", "clock threshold"),
            src("sda", ":TRIG:IIC:SDA", true, "data line"),
            level("sda_level", ":TRIG:IIC:DLEV", "data threshold"),
            en(
                "when",
                ":TRIG:IIC:WHEN",
                &[
                    "STARt",
                    "RESTart",
                    "STOP",
                    "NACKnowledge",
                    "ADDRess",
                    "DATA",
                    "ADATa",
                ],
                "trigger condition",
            ),
            en(
                "address_width",
                ":TRIG:IIC:AWID",
                &["7", "8", "10"],
                "address bits",
            ),
            int("address", ":TRIG:IIC:ADDR", 0, 1023, "address"),
            en(
                "direction",
                ":TRIG:IIC:DIR",
                &["READ", "WRITe", "RWRite"],
                "transfer direction",
            ),
            int("bytes", ":TRIG:IIC:DBYT", 1, 5, "data bytes"),
            int("data", ":TRIG:IIC:DATA", 0, (1u64 << 40) - 1, "data value"),
        ],
    },
    Group {
        id: "trigger.spi",
        title: "SPI trigger",
        select: Some(":TRIG:MODE SPI"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("clock", ":TRIG:SPI:CLK", true, "clock line"),
            level("clock_level", ":TRIG:SPI:CLEV", "clock threshold"),
            en("slope", ":TRIG:SPI:SLOP", POL, "clock edge"),
            src("miso", ":TRIG:SPI:MISO", true, "data line"),
            level("data_level", ":TRIG:SPI:DLEV", "data threshold"),
            en(
                "when",
                ":TRIG:SPI:WHEN",
                &["CS", "TIMeout"],
                "frame by chip select or timeout",
            ),
            src("cs", ":TRIG:SPI:CS", true, "chip select (WHEN CS)"),
            level("cs_level", ":TRIG:SPI:SLEV", "chip select threshold"),
            en(
                "cs_mode",
                ":TRIG:SPI:MODE",
                &["HIGH", "LOW"],
                "active chip select level",
            ),
            time(
                "timeout",
                ":TRIG:SPI:TIM",
                8e-9,
                "idle timeout (WHEN TIMeout)",
            ),
            int("width", ":TRIG:SPI:WIDT", 4, 32, "data bits"),
            int("data", ":TRIG:SPI:DATA", 0, (1u64 << 32) - 1, "data value"),
        ],
    },
    Group {
        id: "trigger.can",
        title: "CAN trigger",
        select: Some(":TRIG:MODE CAN"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            int("baud", ":TRIG:CAN:BAUD", 10_000, 5_000_000, "bit rate"),
            src("source", ":TRIG:CAN:SOUR", true, "bus line"),
            en(
                "signal",
                ":TRIG:CAN:STYP",
                &["H", "L", "RXTX", "DIFFerential"],
                "signal type",
            ),
            en(
                "when",
                ":TRIG:CAN:WHEN",
                &[
                    "SOF",
                    "EOF",
                    "IDRemote",
                    "OVERload",
                    "IDFRame",
                    "DATaframe",
                    "IDData",
                    "ERFRame",
                    "ERANswer",
                    "ERCHeck",
                    "ERFormat",
                    "ERRandom",
                    "ERBit",
                ],
                "trigger condition",
            ),
            int("sample_point", ":TRIG:CAN:SPO", 10, 90, "sample point in %"),
            boolean("extended", ":TRIG:CAN:EXT", "29-bit identifier"),
            en(
                "define",
                ":TRIG:CAN:DEF",
                &["DATA", "ID"],
                "value defines data or ID",
            ),
            int("data_bytes", ":TRIG:CAN:DWID", 1, 8, "data length"),
            int("data", ":TRIG:CAN:DATA", 0, u64::MAX, "data/ID value"),
            level("level", ":TRIG:CAN:LEV", "threshold"),
        ],
    },
    Group {
        id: "trigger.lin",
        title: "LIN trigger",
        select: Some(":TRIG:MODE LIN"),
        instances: None,
        requires: Requires::Nothing,
        params: &[
            src("source", ":TRIG:LIN:SOUR", true, "bus line"),
            level("level", ":TRIG:LIN:LEV", "threshold"),
            en(
                "standard",
                ":TRIG:LIN:STAN",
                &["1X", "2X", "BOTH"],
                "LIN version",
            ),
            int("baud", ":TRIG:LIN:BAUD", 1000, 20_000_000, "bit rate"),
            int(
                "sample_point",
                ":TRIG:LIN:SAMP",
                10,
                90,
                "sample point in %",
            ),
            en(
                "when",
                ":TRIG:LIN:WHEN",
                &[
                    "SYNCbreak",
                    "ID",
                    "DATA",
                    "IDData",
                    "SLEep",
                    "WAKeup",
                    "ERRor",
                ],
                "trigger condition",
            ),
            en(
                "error",
                ":TRIG:LIN:ERR",
                &["SYNC", "ID", "CHECk"],
                "error type (WHEN ERRor)",
            ),
            int("id", ":TRIG:LIN:ID", 0, 63, "frame identifier"),
            ParamSpec {
                name: "data",
                node: ":TRIG:LIN:DATA",
                kind: ParamKind::Text { max_len: 20 },
                help: "data value; the scope reports don't-care bits as X (e.g. XXXXXXXX)",
            },
        ],
    },
];

// ---- lookup, verification ----------------------------------------------------------------------

pub fn find_group(id: &str) -> Result<&'static Group> {
    GROUPS
        .iter()
        .find(|g| g.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| DriverError::Invalid(format!("unknown settings group {id:?}")))
}

/// Node with the instance substituted.
pub fn node(spec: &ParamSpec, n: u8) -> String {
    spec.node.replace("{n}", &n.to_string())
}

/// Query used to read a parameter (with `source` filled in for source-keyed levels).
pub fn query_for(spec: &ParamSpec, n: u8, source: Option<&str>) -> String {
    match spec.kind {
        ParamKind::SourceLevel { .. } => {
            format!("{}? {}", node(spec, n), source.unwrap_or("CHAN1"))
        }
        _ => format!("{}?", node(spec, n)),
    }
}

/// Verification candidates (queries) for a group, instance 1.
pub fn verification_queries(g: &Group) -> Vec<String> {
    let first = g.instances.map(|(a, _)| a).unwrap_or(1);
    g.params.iter().map(|p| query_for(p, first, None)).collect()
}

#[derive(Deserialize)]
struct VerifiedEntry {
    command: String,
    outcome: String,
}

/// Queries confirmed on the instrument (compiled in from `docs/verified-commands.json`).
fn verified() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        let text = include_str!("../../../docs/verified-commands.json");
        serde_json::from_str::<Vec<VerifiedEntry>>(text)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.outcome == "ok")
            .map(|e| e.command)
            .collect()
    })
}

/// `true` if every query of the group was confirmed on the instrument.
pub fn group_verified(g: &Group) -> bool {
    verification_queries(g)
        .iter()
        .all(|q| verified().contains(q))
}

// ---- values ----------------------------------------------------------------------------------

/// Short form of an SCPI long form: the leading run of non-lower-case characters.
pub fn short_form(long: &str) -> &str {
    let end = long
        .find(|c: char| c.is_ascii_lowercase())
        .unwrap_or(long.len());
    &long[..end]
}

fn enum_token(values: &[&str], input: &str) -> Option<String> {
    let u = input.trim().to_ascii_uppercase();
    values.iter().find_map(|long| {
        let short = short_form(long);
        let full = long.to_ascii_uppercase();
        // SCPI accepts the short form or the long form.
        (u == short || u == full).then(|| short.to_string())
    })
}

fn invalid(spec: &ParamSpec, msg: impl std::fmt::Display) -> DriverError {
    DriverError::Invalid(format!("{}: {msg}", spec.name))
}

/// Validates `v` and renders it as SCPI argument.
pub fn encode(spec: &ParamSpec, v: &ParamValue) -> Result<String> {
    match (spec.kind, v) {
        (ParamKind::Real { min, max, .. }, _) => {
            let x = match v {
                ParamValue::Real(x) => *x,
                ParamValue::Int(i) => *i as f64,
                ParamValue::Text(t) => t.trim().parse::<f64>().map_err(|e| invalid(spec, e))?,
                ParamValue::Bool(_) => return Err(invalid(spec, "expected a number")),
            };
            if !x.is_finite() || x < min || x > max {
                return Err(invalid(spec, format!("{x} outside {min}..={max}")));
            }
            Ok(format!("{x:e}"))
        }
        (ParamKind::Int { min, max }, _) => {
            let i = match v {
                ParamValue::Int(i) => *i,
                ParamValue::Real(x) if x.fract() == 0.0 && *x >= 0.0 => *x as u64,
                ParamValue::Text(t) => t.trim().parse::<u64>().map_err(|e| invalid(spec, e))?,
                _ => return Err(invalid(spec, "expected a non-negative integer")),
            };
            if i < min || i > max {
                return Err(invalid(spec, format!("{i} outside {min}..={max}")));
            }
            Ok(i.to_string())
        }
        (ParamKind::Enum(values), _) => {
            let s = match v {
                ParamValue::Text(t) => t.clone(),
                ParamValue::Int(i) => i.to_string(),
                ParamValue::Real(x) => x.to_string(),
                ParamValue::Bool(_) => {
                    return Err(invalid(spec, "expected one of the listed values"));
                }
            };
            enum_token(values, &s)
                .ok_or_else(|| invalid(spec, format!("{s:?} not one of {values:?}")))
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => Ok(if *b { "ON" } else { "OFF" }.into()),
        (ParamKind::Bool, ParamValue::Text(t)) => match t.trim().to_ascii_uppercase().as_str() {
            "1" | "ON" | "TRUE" => Ok("ON".into()),
            "0" | "OFF" | "FALSE" => Ok("OFF".into()),
            _ => Err(invalid(spec, "expected true/false")),
        },
        (ParamKind::Bool, ParamValue::Int(i)) if *i <= 1 => {
            Ok(if *i == 1 { "ON" } else { "OFF" }.into())
        }
        (ParamKind::Bool, _) => Err(invalid(spec, "expected true/false")),
        (ParamKind::Source { digital }, ParamValue::Text(t)) => {
            let s = crate::model::normalize_source(t)?;
            if s.starts_with("CHAN") || (digital && s.starts_with('D')) {
                Ok(s)
            } else {
                Err(invalid(
                    spec,
                    format!(
                        "{s} not allowed (CHAN1..4{})",
                        if digital { ", D0..D15" } else { "" }
                    ),
                ))
            }
        }
        (ParamKind::Source { .. }, _) => Err(invalid(spec, "expected a source like CHAN1")),
        (ParamKind::Text { max_len }, ParamValue::Text(t)) => {
            let ok = !t.is_empty()
                && t.len() <= max_len
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if ok {
                Ok(t.clone())
            } else {
                Err(invalid(
                    spec,
                    format!("1-{max_len} characters of A-Z a-z 0-9 _ - ."),
                ))
            }
        }
        (ParamKind::Text { .. }, _) => Err(invalid(spec, "expected text")),
        (ParamKind::Pattern { symbols, count }, ParamValue::Text(t)) => {
            let parts: Vec<String> = t
                .split([',', ' '])
                .filter(|p| !p.is_empty())
                .map(|p| p.trim().to_ascii_uppercase())
                .collect();
            let ok = !parts.is_empty()
                && parts.len() <= count
                && parts
                    .iter()
                    .all(|p| p.len() == 1 && symbols.contains(p.as_str()));
            if ok {
                Ok(parts.join(","))
            } else {
                Err(invalid(
                    spec,
                    format!("up to {count} of {symbols}, comma-separated"),
                ))
            }
        }
        (ParamKind::Pattern { .. }, _) => Err(invalid(spec, "expected a pattern like H,L,X,X")),
        (ParamKind::SourceLevel { .. }, _) => {
            // Encoded together with the source in `write_command`.
            let x = match v {
                ParamValue::Real(x) => *x,
                ParamValue::Int(i) => *i as f64,
                _ => return Err(invalid(spec, "expected a number")),
            };
            if !x.is_finite() || x.abs() > 1000.0 {
                return Err(invalid(spec, "level out of range"));
            }
            Ok(format!("{x:e}"))
        }
    }
}

/// Parses an answer according to the parameter kind.
pub fn decode(spec: &ParamSpec, answer: &str) -> Result<ParamValue> {
    let a = answer.trim().trim_matches('"');
    let bad = || DriverError::Protocol(format!("{}: unexpected answer {a:?}", spec.node));
    Ok(match spec.kind {
        ParamKind::Real { .. } | ParamKind::SourceLevel { .. } => {
            ParamValue::Real(a.parse().map_err(|_| bad())?)
        }
        ParamKind::Int { .. } => {
            let x: f64 = a.parse().map_err(|_| bad())?;
            if x < 0.0 {
                return Err(bad());
            }
            // Large values (e.g. 64-bit CAN data) must not go through f64.
            ParamValue::Int(a.parse::<u64>().unwrap_or(x.round() as u64))
        }
        ParamKind::Bool => match a.to_ascii_uppercase().as_str() {
            "1" | "ON" => ParamValue::Bool(true),
            "0" | "OFF" => ParamValue::Bool(false),
            _ => return Err(bad()),
        },
        _ => ParamValue::Text(a.to_string()),
    })
}

/// SCPI command writing `value` (already encoded) to `spec`.
pub fn write_command(spec: &ParamSpec, n: u8, encoded: &str, source: Option<&str>) -> String {
    match spec.kind {
        ParamKind::SourceLevel { .. } => {
            format!("{} {},{encoded}", node(spec, n), source.unwrap_or("CHAN1"))
        }
        _ => format!("{} {encoded}", node(spec, n)),
    }
}

// ---- API descriptions ------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ParamInfo {
    pub name: String,
    pub node: String,
    /// `real`, `int`, `enum`, `bool`, `source`, `text`, `pattern`, `source_level`.
    pub kind: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub unit: Option<String>,
    /// Accepted values (long forms; the short form is the upper-case part).
    pub values: Vec<String>,
    pub help: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct GroupInfo {
    pub id: String,
    pub title: String,
    /// Instance range, e.g. `[1, 4]` for decoders.
    pub instances: Option<(u8, u8)>,
    pub requires: Requires,
    /// All queries of this group were confirmed on the instrument.
    pub verified: bool,
    /// Hardware present (see `requires`).
    pub available: bool,
    pub params: Vec<ParamInfo>,
}

pub fn describe(g: &Group, available: bool) -> GroupInfo {
    GroupInfo {
        id: g.id.into(),
        title: g.title.into(),
        instances: g.instances,
        requires: g.requires,
        verified: group_verified(g),
        available,
        params: g
            .params
            .iter()
            .map(|p| {
                let (kind, min, max, unit, values): (
                    &str,
                    Option<f64>,
                    Option<f64>,
                    Option<String>,
                    Vec<String>,
                ) = match p.kind {
                    ParamKind::Real { min, max, unit } => {
                        ("real", Some(min), Some(max), Some(unit.into()), vec![])
                    }
                    ParamKind::Int { min, max } => {
                        ("int", Some(min as f64), Some(max as f64), None, vec![])
                    }
                    ParamKind::Enum(v) => (
                        "enum",
                        None,
                        None,
                        None,
                        v.iter().map(|s| s.to_string()).collect(),
                    ),
                    ParamKind::Bool => ("bool", None, None, None, vec![]),
                    ParamKind::Source { digital } => {
                        let mut v: Vec<String> = (1..=4).map(|n| format!("CHAN{n}")).collect();
                        if digital {
                            v.extend((0..16).map(|n| format!("D{n}")));
                        }
                        ("source", None, None, None, v)
                    }
                    ParamKind::Text { max_len } => {
                        ("text", None, Some(max_len as f64), None, vec![])
                    }
                    ParamKind::Pattern { symbols, count } => (
                        "pattern",
                        None,
                        Some(count as f64),
                        None,
                        symbols.chars().map(|c| c.to_string()).collect(),
                    ),
                    ParamKind::SourceLevel { .. } => {
                        ("source_level", None, None, Some("V".into()), vec![])
                    }
                };
                ParamInfo {
                    name: p.name.into(),
                    node: p.node.into(),
                    kind: kind.into(),
                    min,
                    max,
                    unit,
                    values,
                    help: p.help.into(),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_forms() {
        assert_eq!(short_form("POSitive"), "POS");
        assert_eq!(short_form("GLESs"), "GLES");
        assert_eq!(short_form("720P60"), "720P60");
        assert_eq!(short_form("DATaframe"), "DAT");
        assert_eq!(short_form("1.5"), "1.5");
    }

    #[test]
    fn tables_are_consistent() {
        let mut ids = HashSet::new();
        for g in GROUPS {
            assert!(ids.insert(g.id), "duplicate group {}", g.id);
            let mut names = HashSet::new();
            for p in g.params {
                assert!(names.insert(p.name), "duplicate param {}.{}", g.id, p.name);
                assert!(p.node.starts_with(':'), "{}", p.node);
                if let ParamKind::SourceLevel { source_param } = p.kind {
                    assert!(
                        g.params.iter().any(|q| q.name == source_param),
                        "{}.{}",
                        g.id,
                        p.name
                    );
                }
                if g.instances.is_none() {
                    assert!(
                        !p.node.contains("{n}"),
                        "{}: {{n}} without instances",
                        p.node
                    );
                }
            }
        }
    }

    #[test]
    fn encoding_and_validation() {
        let g = find_group("trigger.pulse").unwrap();
        let p = |n: &str| g.params.iter().find(|x| x.name == n).unwrap();
        assert_eq!(
            encode(p("polarity"), &ParamValue::Text("negative".into())).unwrap(),
            "NEG"
        );
        assert_eq!(
            encode(p("when"), &ParamValue::Text("GLES".into())).unwrap(),
            "GLES"
        );
        assert!(encode(p("when"), &ParamValue::Text("sometimes".into())).is_err());
        assert_eq!(encode(p("lower"), &ParamValue::Real(2e-6)).unwrap(), "2e-6");
        assert!(encode(p("lower"), &ParamValue::Real(20.0)).is_err());
        assert_eq!(
            encode(p("source"), &ParamValue::Text("d3".into())).unwrap(),
            "D3"
        );
        let slope = find_group("trigger.slope").unwrap();
        assert!(
            encode(&slope.params[0], &ParamValue::Text("D3".into())).is_err(),
            "analog only"
        );
        let pat = &find_group("trigger.pattern").unwrap().params[0];
        assert_eq!(
            encode(pat, &ParamValue::Text("h, l, x,r".into())).unwrap(),
            "H,L,X,R"
        );
        assert!(encode(pat, &ParamValue::Text("H,Q".into())).is_err());
        let can = find_group("trigger.can").unwrap();
        let data = can.params.iter().find(|x| x.name == "data").unwrap();
        assert_eq!(
            decode(data, "18446744073709551615").unwrap(),
            ParamValue::Int(u64::MAX)
        );
        let dbyt = &find_group("trigger.i2c").unwrap().params[8];
        assert_eq!(decode(dbyt, "3.000000E+00").unwrap(), ParamValue::Int(3));
    }
}
