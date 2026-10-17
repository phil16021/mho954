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
}

struct Fixed {
    group: &'static str,
    command: &'static str,
    kind: Kind,
    note: &'static str,
}

const fn q(group: &'static str, command: &'static str, note: &'static str) -> Fixed {
    Fixed {
        group,
        command,
        kind: Kind::Query,
        note,
    }
}

const fn c(group: &'static str, command: &'static str, note: &'static str) -> Fixed {
    Fixed {
        group,
        command,
        kind: Kind::Command,
        note,
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
    q("bus.data", ":BUS1:DATA?", "guide 3.4.6 (block + text)"),
    c("bus.data", ":BUS1:EVEN OFF", "restore"),
    c("bus.data", ":BUS1:DISP OFF", "restore"),
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
        })
        .collect();
    for g in crate::params::GROUPS {
        for q in crate::params::verification_queries(g) {
            out.push(Candidate {
                group: g.id.into(),
                command: q,
                kind: Kind::Query,
                note: g.title.into(),
            });
        }
    }
    out
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
    let timeout = Duration::from_millis(1500);
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
