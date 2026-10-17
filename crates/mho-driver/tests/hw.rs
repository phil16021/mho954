//! Hardware tests against a real MHO954 over USB. Run with
//! `MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1`.
//!
//! Only verified commands are used, and every step is followed by `*IDN?` so a problem is
//! caught immediately (the firmware hangs after ~5 unknown queries per power cycle).

use std::time::Duration;

use mho_driver::{Connection, Scope, WaveMode};

fn scope() -> Option<Scope> {
    if std::env::var_os("MHO954_HW").is_none() {
        eprintln!("skipped: set MHO954_HW=1");
        return None;
    }
    Some(Scope::open(&Connection::Usb { serial: None }).expect("open scope"))
}

fn healthy(s: &mut Scope) {
    let idn = s.scpi().query("*IDN?").expect("scope stopped answering");
    assert!(idn.contains("MHO9"), "{idn}");
}

#[test]
#[ignore]
fn identify_status_and_waveform() {
    let Some(mut s) = scope() else { return };
    assert!(s.idn().model.starts_with("MHO9"));
    let st = s.status().unwrap();
    assert_eq!(st.channels.len(), 4);
    healthy(&mut s);
    let pre = s.preamble("CHAN1", WaveMode::Normal).unwrap();
    assert_eq!(pre.points, 1000);
    assert_eq!(pre.y_reference, 32768.0);
    let w = s.screen_waveform("CHAN1", Some(pre)).unwrap();
    assert_eq!(w.raw.len(), 1000);
    healthy(&mut s);
}

#[test]
#[ignore]
fn screenshot_and_measure() {
    let Some(mut s) = scope() else { return };
    let png = s.screenshot().unwrap();
    assert!(png.len() > 10_000);
    healthy(&mut s);
    // Invalid values (no signal) are fine; the query must be answered.
    s.measure("VPP", "CHAN1", None).unwrap();
    healthy(&mut s);
}

#[test]
#[ignore]
fn recovers_from_one_unanswered_query() {
    let Some(mut s) = scope() else { return };
    // Costs one of the ~5 unknown queries the firmware tolerates per power cycle.
    let err = s
        .raw(":NOPE:NOTHING?", Duration::from_millis(800))
        .unwrap_err();
    assert!(err.to_string().contains("no answer"), "{err}");
    healthy(&mut s);
}
