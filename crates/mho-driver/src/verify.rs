//! Hardware verification of SCPI commands that are documented in the programming guide but
//! not yet confirmed on the instrument.
//!
//! The MHO954 never answers an unknown query and its firmware hangs for good after about five
//! of them per power cycle. Every candidate is therefore sent alone, followed by `*IDN?` as a
//! health check, and unanswered queries are counted against a budget the caller persists.
//! Set commands cost nothing: errors land in the error queue.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::scope::Scope;

/// Unanswered queries tolerated per power cycle before asking for a restart (the firmware
/// hangs after ~5).
pub const UNKNOWN_QUERY_BUDGET: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Must be answered.
    Query,
    /// Must leave the error queue empty.
    Command,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub group: String,
    pub command: String,
    pub kind: Kind,
    /// Guide reference or reason.
    pub note: String,
    /// How long to wait for an answer.
    pub timeout: Duration,
    /// Hand-picked probe (changes settings, may cost budget): only run when its group is named
    /// exactly, never as a subgroup of a broader selection (`verify bus` skips `bus.data`).
    pub explicit: bool,
}

/// Answer timeout for settings read-backs.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);
/// Answer timeout for queries that make the scope compile a table (decoder events, FFT peaks).
pub const SLOW_QUERY_TIMEOUT: Duration = Duration::from_secs(8);

struct Fixed {
    group: &'static str,
    command: &'static str,
    kind: Kind,
    note: &'static str,
    slow: bool,
}

const fn q(group: &'static str, command: &'static str, note: &'static str) -> Fixed {
    Fixed {
        group,
        command,
        kind: Kind::Query,
        note,
        slow: false,
    }
}

/// A query that may take several seconds to answer.
const fn qs(group: &'static str, command: &'static str, note: &'static str) -> Fixed {
    Fixed {
        group,
        command,
        kind: Kind::Query,
        note,
        slow: true,
    }
}

const fn c(group: &'static str, command: &'static str, note: &'static str) -> Fixed {
    Fixed {
        group,
        command,
        kind: Kind::Command,
        note,
        slow: false,
    }
}

/// Hand-picked candidates. Commands that change settings restore the default afterwards.
const FIXED: &[Fixed] = &[
    q("options", ":SYST:OPT:STAT? BWU05T08", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? BWU03T05", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? BWU03T08", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? RLU-05", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? AFG50", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? AFG100", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? CAN-FD", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? FLEX", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? AUDio", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? AERO", "guide 3.24.18"),
    q("options", ":SYST:OPT:STAT? BND", "guide 3.24.18"),
    q(
        "system",
        ":SYST:MOD?",
        "guide 3.24 :SYSTem:MODules? (LA/AFG hardware present)",
    ),
    q("system", "*ESR?", "guide IEEE 488.2"),
    q("system", "*STB?", "guide IEEE 488.2"),
    c("system", "*CLS", "guide IEEE 488.2"),
    q(
        "compound",
        ":TIM:SCAL?;:CHAN1:SCAL?",
        "two queries in one message",
    ),
    c(
        "digital",
        ":WAV:SOUR D0",
        "D0..D15 as waveform source (not in the guide's list)",
    ),
    c("digital", ":WAV:SOUR CHAN1", "restore source"),
    c(
        "measure",
        ":MEAS:ITEM VPP,CHAN1",
        "set form creates the measurement",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? CURR,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? DEV,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? AVER,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? MIN,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? MAX,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q(
        "measure.stats",
        ":MEAS:STAT:ITEM? CNT,VPP,CHAN1",
        "guide 3.17.8",
    ),
    q("counter.value", ":COUN:CURR?", "guide 3.7.1"),
    q("dvm.value", ":DVM:CURR?", "guide 3.10.1"),
    // The event table is only read with the decoder and its table on; restored afterwards.
    c("bus.data", ":BUS1:DISP ON", "decoder 1 on (guide 3.4.2)"),
    c("bus.data", ":BUS1:EVEN ON", "event table on (guide 3.4.4)"),
    qs("bus.data", ":BUS1:DATA?", "guide 3.4.6 (block + text)"),
    c("bus.data", ":BUS1:EVEN OFF", "restore"),
    c("bus.data", ":BUS1:DISP OFF", "restore"),
    // Peak results with FFT and peak search on; restored afterwards.
    c(
        "math.peaks",
        ":MATH1:OPER FFT",
        "math 1 as FFT (guide 3.16.2)",
    ),
    c("math.peaks", ":MATH1:DISP ON", "math 1 on"),
    c(
        "math.peaks",
        ":MATH1:FFT:SEAR:ENAB ON",
        "peak search on (guide 3.16.25)",
    ),
    qs(
        "math.peaks",
        ":MATH1:FFT:SEAR:RES?",
        "guide 3.16.30 (several lines)",
    ),
    c("math.peaks", ":MATH1:FFT:SEAR:ENAB OFF", "restore"),
    c("math.peaks", ":MATH1:DISP OFF", "restore"),
    c("math.peaks", ":MATH1:OPER ADD", "restore"),
];

/// All candidates: the fixed list plus every query of every settings group
/// ([`crate::params::GROUPS`]); the group id (e.g. `trigger.pulse`) is the verify group.
pub fn candidates() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = FIXED
        .iter()
        .map(|f| Candidate {
            group: f.group.into(),
            command: f.command.into(),
            kind: f.kind,
            note: f.note.into(),
            timeout: if f.slow {
                SLOW_QUERY_TIMEOUT
            } else {
                QUERY_TIMEOUT
            },
            explicit: true,
        })
        .collect();
    for g in crate::params::GROUPS {
        for q in crate::params::verification_queries(g) {
            out.push(Candidate {
                group: g.id.into(),
                command: q,
                kind: Kind::Query,
                note: g.title.into(),
                timeout: QUERY_TIMEOUT,
                explicit: false,
            });
        }
    }
    // Readouts: switch the mode on, read, restore.
    for r in crate::ops::READOUTS {
        let cmd = |command: &str, note: &str| Candidate {
            group: r.id.into(),
            command: command.into(),
            kind: Kind::Command,
            note: note.into(),
            timeout: QUERY_TIMEOUT,
            explicit: false,
        };
        out.extend(r.setup.iter().map(|c| cmd(c, "enable the mode")));
        for q in crate::ops::readout_queries(r) {
            out.push(Candidate {
                group: r.id.into(),
                command: q,
                kind: Kind::Query,
                note: r.title.into(),
                timeout: QUERY_TIMEOUT,
                explicit: false,
            });
        }
        out.extend(r.restore.iter().map(|c| cmd(c, "restore")));
    }
    out
}

/// The candidates `verify <groups>` runs: a named group plus its subgroups (`bus` →
/// `bus.rs232`, …), except hand-picked probes, which need their exact name. No names: all.
pub fn select(all: Vec<Candidate>, groups: &[String]) -> Vec<Candidate> {
    all.into_iter()
        .filter(|c| {
            groups.is_empty()
                || groups.iter().any(|g| {
                    *g == c.group || (!c.explicit && c.group.starts_with(&format!("{g}.")))
                })
        })
        .collect()
}

pub fn groups() -> Vec<String> {
    let mut g: Vec<String> = Vec::new();
    for c in candidates() {
        if !g.contains(&c.group) {
            g.push(c.group);
        }
    }
    g
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Answered (query) or accepted without error (command).
    Ok,
    /// Query went unanswered (costs budget).
    NoAnswer,
    /// Command produced error-queue entries.
    Rejected,
    /// The health check after the command failed: stop immediately.
    ScopeUnresponsive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResult {
    pub command: String,
    pub group: String,
    pub outcome: Outcome,
    /// Response line (queries) or error-queue text (commands).
    pub detail: String,
}

/// Verifies one candidate. `Outcome::ScopeUnresponsive` means: stop and power-cycle.
pub fn check(scope: &mut Scope, cand: &Candidate) -> Result<VerifyResult> {
    let timeout = cand.timeout;
    let (outcome, detail) = match cand.kind {
        Kind::Query => match scope.raw(&cand.command, timeout) {
            Ok(r) => (Outcome::Ok, r.unwrap_or_default()),
            Err(e) if e.to_string().contains("power-cycle") => {
                (Outcome::ScopeUnresponsive, e.to_string())
            }
            Err(e) if e.to_string().contains("no answer") => (Outcome::NoAnswer, e.to_string()),
            Err(e) => return Err(e),
        },
        Kind::Command => match scope.raw(&cand.command, timeout) {
            Ok(_) => (Outcome::Ok, String::new()),
            Err(crate::DriverError::Device(msg)) => (Outcome::Rejected, msg),
            Err(e) => return Err(e),
        },
    };
    // Health check: the scope must still answer a known query.
    let outcome = match scope.scpi().query("*IDN?") {
        Ok(_) => outcome,
        Err(_) => Outcome::ScopeUnresponsive,
    };
    Ok(VerifyResult {
        command: cand.command.clone(),
        group: cand.group.clone(),
        outcome,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(groups: &[&str]) -> Vec<String> {
        let g: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
        let mut out: Vec<String> = Vec::new();
        for c in select(candidates(), &g) {
            if !out.contains(&c.group) {
                out.push(c.group);
            }
        }
        out
    }

    #[test]
    fn probes_need_their_exact_name() {
        let bus = names(&["bus"]);
        assert!(bus.contains(&"bus".to_string()));
        assert!(bus.contains(&"bus.rs232".to_string()));
        assert!(!bus.contains(&"bus.data".to_string()));
        assert_eq!(names(&["bus.data"]), ["bus.data"]);
        assert!(!names(&["math"]).contains(&"math.peaks".to_string()));
        assert!(names(&[]).contains(&"math.peaks".to_string()));
    }

    #[test]
    fn table_queries_wait_longer() {
        let all = candidates();
        let t = |cmd: &str| all.iter().find(|c| c.command == cmd).unwrap().timeout;
        assert_eq!(t(":BUS1:DATA?"), SLOW_QUERY_TIMEOUT);
        assert_eq!(t(":MATH1:FFT:SEAR:RES?"), SLOW_QUERY_TIMEOUT);
        assert_eq!(t(":BUS1:MODE?"), QUERY_TIMEOUT);
    }
}
