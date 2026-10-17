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

#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub group: &'static str,
    pub command: &'static str,
    pub kind: Kind,
    /// Guide reference or reason.
    pub note: &'static str,
}

const fn q(group: &'static str, command: &'static str, note: &'static str) -> Candidate {
    Candidate {
        group,
        command,
        kind: Kind::Query,
        note,
    }
}

const fn c(group: &'static str, command: &'static str, note: &'static str) -> Candidate {
    Candidate {
        group,
        command,
        kind: Kind::Command,
        note,
    }
}

/// All candidates. Commands that change settings restore the default afterwards.
pub const CANDIDATES: &[Candidate] = &[
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
];

pub fn groups() -> Vec<&'static str> {
    let mut g: Vec<&str> = CANDIDATES.iter().map(|c| c.group).collect();
    g.dedup();
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
        Kind::Query => match scope.raw(cand.command, timeout) {
            Ok(r) => (Outcome::Ok, r.unwrap_or_default()),
            Err(e) if e.to_string().contains("power-cycle") => {
                (Outcome::ScopeUnresponsive, e.to_string())
            }
            Err(e) if e.to_string().contains("no answer") => (Outcome::NoAnswer, e.to_string()),
            Err(e) => return Err(e),
        },
        Kind::Command => match scope.raw(cand.command, timeout) {
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
        command: cand.command.into(),
        group: cand.group.into(),
        outcome,
        detail,
    })
}
