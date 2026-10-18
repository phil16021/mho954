//! Read-only values (cursor readings, mask/search/histogram results) and one-shot commands
//! (save/load on the scope, resets, recording) — the counterparts of the settings groups in
//! [`crate::params`].
//!
//! Readout queries are only answered in the matching mode (e.g. cursor values while the
//! cursor mode is MANual), and an unanswered query costs the scope's unknown-query budget.
//! Each readout therefore has a guard query that is checked first; verification switches the
//! mode on with `setup` and restores it with `restore`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{DriverError, Result};
use crate::params::{self, ParamValue};

#[derive(Debug, Clone, Copy)]
pub enum ReadKind {
    Real {
        unit: &'static str,
    },
    Int,
    Text,
    /// `[Key:value,Key:value,…]`, e.g. the histogram statistics.
    KeyValues,
}

#[derive(Debug, Clone, Copy)]
pub struct ReadItem {
    pub name: &'static str,
    pub query: &'static str,
    pub kind: ReadKind,
}

#[derive(Debug, Clone, Copy)]
pub struct Readout {
    pub id: &'static str,
    pub title: &'static str,
    /// Query and the expected answer prefix (short form) that make the items answerable.
    pub guard: Option<(&'static str, &'static str)>,
    /// Commands that enable the mode for verification, and restore afterwards.
    pub setup: &'static [&'static str],
    pub restore: &'static [&'static str],
    pub items: &'static [ReadItem],
}

const fn real(name: &'static str, query: &'static str, unit: &'static str) -> ReadItem {
    ReadItem {
        name,
        query,
        kind: ReadKind::Real { unit },
    }
}

const fn int(name: &'static str, query: &'static str) -> ReadItem {
    ReadItem {
        name,
        query,
        kind: ReadKind::Int,
    }
}

pub const READOUTS: &[Readout] = &[
    Readout {
        id: "cursor.manual",
        title: "Manual cursor values (cursor mode MANual)",
        guard: Some((":CURS:MODE?", "MAN")),
        setup: &[":CURS:MODE MAN"],
        restore: &[":CURS:MODE OFF"],
        items: &[
            real("ax", ":CURS:MAN:AXV?", "s"),
            real("ay", ":CURS:MAN:AYV?", ""),
            real("bx", ":CURS:MAN:BXV?", "s"),
            real("by", ":CURS:MAN:BYV?", ""),
            real("dx", ":CURS:MAN:XDEL?", "s"),
            real("inv_dx", ":CURS:MAN:IXD?", "Hz"),
            real("dy", ":CURS:MAN:YDEL?", ""),
        ],
    },
    Readout {
        id: "cursor.track",
        title: "Tracking cursor values (cursor mode TRACk)",
        guard: Some((":CURS:MODE?", "TRAC")),
        setup: &[":CURS:MODE TRAC"],
        restore: &[":CURS:MODE OFF"],
        items: &[
            real("ax", ":CURS:TRAC:AXV?", "s"),
            real("ay", ":CURS:TRAC:AYV?", ""),
            real("bx", ":CURS:TRAC:BXV?", "s"),
            real("by", ":CURS:TRAC:BYV?", ""),
            real("dx", ":CURS:TRAC:XDEL?", "s"),
            real("inv_dx", ":CURS:TRAC:IXD?", "Hz"),
            real("dy", ":CURS:TRAC:YDEL?", ""),
        ],
    },
    Readout {
        id: "mask",
        title: "Mask test results (mask test enabled)",
        guard: Some((":MASK:ENAB?", "1")),
        setup: &[":MASK:ENAB ON"],
        restore: &[":MASK:ENAB OFF"],
        items: &[
            int("failed", ":MASK:FAIL?"),
            int("passed", ":MASK:PASS?"),
            int("total", ":MASK:TOT?"),
        ],
    },
    Readout {
        id: "search",
        title: "Number of found events (search enabled)",
        guard: Some((":SEAR:STAT?", "1")),
        setup: &[":SEAR:STAT ON"],
        restore: &[":SEAR:STAT OFF"],
        items: &[int("count", ":SEAR:COUN?")],
    },
    Readout {
        id: "histogram",
        title: "Histogram statistics (histogram enabled)",
        guard: Some((":HIST:ENAB?", "1")),
        setup: &[":HIST:ENAB ON"],
        restore: &[":HIST:ENAB OFF"],
        items: &[ReadItem {
            name: "statistics",
            query: ":HIST:STAT:RES?",
            kind: ReadKind::KeyValues,
        }],
    },
    Readout {
        id: "save",
        title: "Status of the last save on the scope (1 = done)",
        guard: None,
        setup: &[],
        restore: &[],
        items: &[int("done", ":SAVE:STAT?")],
    },
];

/// Argument of a one-shot command.
#[derive(Debug, Clone, Copy)]
pub enum CommandArg {
    None,
    /// File on the scope: `C:/` (internal) or `D:/` (USB stick) with one of the extensions.
    Path {
        extensions: &'static [&'static str],
    },
    /// Index such as the reference number.
    Index {
        min: u8,
        max: u8,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub id: &'static str,
    pub title: &'static str,
    pub command: &'static str,
    pub arg: CommandArg,
    /// Destructive: only with explicit confirmation.
    pub confirm: bool,
}

const fn cmd(id: &'static str, command: &'static str, title: &'static str) -> Command {
    Command {
        id,
        title,
        command,
        arg: CommandArg::None,
        confirm: false,
    }
}

const fn save(
    id: &'static str,
    command: &'static str,
    extensions: &'static [&'static str],
    title: &'static str,
) -> Command {
    Command {
        id,
        title,
        command,
        arg: CommandArg::Path { extensions },
        confirm: false,
    }
}

pub const COMMANDS: &[Command] = &[
    save(
        "save.setup",
        ":SAVE:SET",
        &["stp"],
        "save the setup to a file on the scope",
    ),
    save(
        "save.waveform",
        ":SAVE:WAV",
        &["csv", "bin", "wfm"],
        "save the screen waveforms to a file on the scope",
    ),
    save(
        "save.memory",
        ":SAVE:MEM:WAV",
        &["bin", "csv"],
        "save the memory waveform to a file on the scope",
    ),
    save(
        "save.image",
        ":SAVE:IMAG",
        &["png", "bmp", "jpg"],
        "save a screenshot to a file on the scope",
    ),
    save(
        "save.mask",
        ":SAVE:MASK",
        &["pf"],
        "save the mask to a file on the scope",
    ),
    save(
        "load.setup",
        ":LOAD:SET",
        &["stp"],
        "load a setup file stored on the scope",
    ),
    save(
        "load.mask",
        ":LOAD:MASK",
        &["pf"],
        "load a mask file stored on the scope",
    ),
    save(
        "histogram.save",
        ":HIST:SAVE:CSV",
        &["csv"],
        "save the histogram data on the scope",
    ),
    cmd(
        "display.clear",
        ":DISP:CLE",
        "clear the waveforms on screen",
    ),
    cmd(
        "mask.create",
        ":MASK:CRE",
        "create the mask from the current waveform",
    ),
    cmd("mask.reset", ":MASK:RES", "reset the mask test counters"),
    cmd(
        "histogram.reset",
        ":HIST:RES",
        "reset the histogram statistics",
    ),
    Command {
        id: "ref.save",
        title: "store the selected source into reference <1..10>",
        command: ":REF:SAVE",
        arg: CommandArg::Index { min: 1, max: 10 },
        confirm: false,
    },
    Command {
        id: "ref.reset",
        title: "reset reference <1..10>",
        command: ":REF:RES",
        arg: CommandArg::Index { min: 1, max: 10 },
        confirm: false,
    },
    cmd("record.start", ":REC:STAR ON", "start recording"),
    cmd("record.stop", ":REC:STAR OFF", "stop recording"),
    cmd("record.play", ":REC:PLAY ON", "play back the recording"),
    cmd("record.pause", ":REC:PLAY OFF", "stop playback"),
    Command {
        id: "reset",
        title: "factory default settings (*RST); back up the setup first",
        command: "*RST",
        arg: CommandArg::None,
        confirm: true,
    },
];

pub fn find_readout(id: &str) -> Result<&'static Readout> {
    READOUTS
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| DriverError::Invalid(format!("unknown readout {id:?}")))
}

pub fn find_command(id: &str) -> Result<&'static Command> {
    COMMANDS
        .iter()
        .find(|c| c.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| DriverError::Invalid(format!("unknown command {id:?}")))
}

/// All queries a readout sends (guard first).
pub fn readout_queries(r: &Readout) -> Vec<String> {
    r.guard
        .map(|(q, _)| q.to_string())
        .into_iter()
        .chain(r.items.iter().map(|i| i.query.to_string()))
        .collect()
}

pub fn readout_verified(r: &Readout) -> bool {
    readout_queries(r).iter().all(|q| params::query_verified(q))
}

/// `true` if `answer` satisfies a guard expecting `expect` (short form, case-insensitive).
pub fn guard_ok(answer: &str, expect: &str) -> bool {
    let a = answer.trim().trim_matches('"').to_ascii_uppercase();
    match expect {
        "1" => a == "1" || a == "ON",
        e => a.starts_with(e),
    }
}

/// Validates a file path on the scope: `C:/` or `D:/`, safe characters, file name ≤ 26
/// characters (guide 3.21), one of `extensions`.
pub fn validate_path(path: &str, extensions: &[&str]) -> Result<String> {
    let p = path.trim().replace('\\', "/");
    let bad = |m: &str| DriverError::Invalid(format!("path {path:?}: {m}"));
    let drive = p.get(..3).map(str::to_ascii_uppercase);
    if !matches!(drive.as_deref(), Some("C:/") | Some("D:/")) {
        return Err(bad("must start with C:/ (internal) or D:/ (USB stick)"));
    }
    let rest = &p[3..];
    if rest.is_empty()
        || rest
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        || !rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return Err(bad("use letters, digits, _ - . and / only"));
    }
    let name = rest.rsplit('/').next().unwrap_or(rest);
    if name.len() > 26 {
        return Err(bad("file name longer than 26 characters"));
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !extensions.contains(&ext.as_str()) {
        return Err(bad(&format!("extension must be one of {extensions:?}")));
    }
    Ok(format!("{}{rest}", p[..3].to_ascii_uppercase()))
}

/// Parses `[Sum:5.6khits,Peaks:14hits,Bin width:20ns,…]` into key/value pairs.
pub fn parse_key_values(text: &str) -> Vec<(String, String)> {
    text.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Converts one readout answer.
pub fn decode_item(item: &ReadItem, answer: &str) -> Result<Vec<(String, ParamValue)>> {
    let a = answer.trim().trim_matches('"');
    let bad = || DriverError::Protocol(format!("{}: unexpected answer {a:?}", item.query));
    Ok(match item.kind {
        ReadKind::Real { .. } => {
            vec![(
                item.name.into(),
                ParamValue::Real(a.parse().map_err(|_| bad())?),
            )]
        }
        ReadKind::Int => {
            let x: f64 = a.parse().map_err(|_| bad())?;
            if x < 0.0 {
                return Err(bad());
            }
            vec![(item.name.into(), ParamValue::Int(x.round() as u64))]
        }
        ReadKind::Text => vec![(item.name.into(), ParamValue::Text(a.to_string()))],
        ReadKind::KeyValues => parse_key_values(a)
            .into_iter()
            .map(|(k, v)| {
                let key = format!("{}.{}", item.name, k.to_ascii_lowercase().replace(' ', "_"));
                (key, ParamValue::Text(v))
            })
            .collect(),
    })
}

// ---- API descriptions ------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct ReadoutInfo {
    pub id: String,
    pub title: String,
    /// Items (name → unit, empty for counts and text).
    pub items: Vec<(String, String)>,
    /// All queries confirmed on the instrument (or the simulator is trusted).
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct CommandInfo {
    pub id: String,
    pub title: String,
    /// Allowed file extensions if the command takes a path on the scope.
    pub path_extensions: Option<Vec<String>>,
    /// Index range if the command takes a number (e.g. reference 1..10).
    pub index: Option<(u8, u8)>,
    /// Must be confirmed explicitly (`confirm: true`).
    pub confirm: bool,
}

pub fn describe_readout(r: &Readout, trusted: bool) -> ReadoutInfo {
    ReadoutInfo {
        id: r.id.into(),
        title: r.title.into(),
        items: r
            .items
            .iter()
            .map(|i| {
                let unit = match i.kind {
                    ReadKind::Real { unit } => unit,
                    _ => "",
                };
                (i.name.to_string(), unit.to_string())
            })
            .collect(),
        verified: trusted || readout_verified(r),
    }
}

pub fn describe_command(c: &Command) -> CommandInfo {
    CommandInfo {
        id: c.id.into(),
        title: c.title.into(),
        path_extensions: match c.arg {
            CommandArg::Path { extensions } => {
                Some(extensions.iter().map(|e| e.to_string()).collect())
            }
            _ => None,
        },
        index: match c.arg {
            CommandArg::Index { min, max } => Some((min, max)),
            _ => None,
        },
        confirm: c.confirm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_statistics_as_sent_by_the_scope() {
        // Firmware 00.01.00, histogram on without data.
        let a = "[Sum:0hits, Peaks:0hits, Max:0, Min:0, Pk_Pk:0, Mean:0, Median:0, Mode:0, \
                 Bin width:0, Sigma:0, meanPlusSigma:*****, meanPlus2Sigma:*****, \
                 meanPlus3Sigma:*****]";
        let kv = parse_key_values(a);
        assert_eq!(kv.len(), 13);
        assert_eq!(kv[0], ("Sum".into(), "0hits".into()));
        assert_eq!(kv[8], ("Bin width".into(), "0".into()));
        assert_eq!(kv[12], ("meanPlus3Sigma".into(), "*****".into()));
    }

    #[test]
    fn paths() {
        assert_eq!(
            validate_path("d:/setups/bench1.stp", &["stp"]).unwrap(),
            "D:/setups/bench1.stp"
        );
        assert_eq!(validate_path("C:\\a.STP", &["stp"]).unwrap(), "C:/a.STP");
        for bad in [
            "E:/a.stp",
            "/etc/passwd",
            "C:/",
            "C:/../a.stp",
            "C:/a b.stp",
            "C:/a.png",
            "C:/a;:SYST:RES.stp",
            "C:/abcdefghijklmnopqrstuvwxyz.stp",
        ] {
            assert!(validate_path(bad, &["stp"]).is_err(), "{bad}");
        }
    }

    #[test]
    fn guards_and_values() {
        assert!(guard_ok("MAN", "MAN"));
        assert!(guard_ok("ON", "1"));
        assert!(!guard_ok("0", "1"));
        assert!(!guard_ok("OFF", "MAN"));
        let kv = decode_item(
            &READOUTS[4].items[0],
            "[Sum:5.6khits,Peaks:14hits,Bin width:20ns,Siqma:2.303us]",
        )
        .unwrap();
        assert_eq!(kv.len(), 4);
        assert_eq!(kv[2].0, "statistics.bin_width");
        assert_eq!(kv[2].1, ParamValue::Text("20ns".into()));
        let n = decode_item(&READOUTS[2].items[0], "12").unwrap();
        assert_eq!(n[0].1, ParamValue::Int(12));
    }

    #[test]
    fn tables_are_consistent() {
        for r in READOUTS {
            assert!(find_readout(r.id).is_ok());
            assert!(r.items.iter().all(|i| i.query.ends_with('?')));
        }
        for c in COMMANDS {
            assert!(!c.command.contains('?'), "{}", c.id);
        }
        assert!(find_command("reset").unwrap().confirm);
    }
}
