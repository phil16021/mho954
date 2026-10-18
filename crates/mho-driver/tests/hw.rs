//! Hardware tests against a real MHO954 (USB unless `MHO954_HW_CONN` names another
//! connection, e.g. `tcp:192.168.1.109`). Run with
//! `MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1`.
//!
//! Only verified commands are used, and every step is followed by `*IDN?` so a problem is
//! caught immediately (the firmware hangs after ~5 unknown queries per power cycle).
//!
//! Generator tests drive real signals and need the bench cabling:
//! - `MHO954_HW_AFG=1`: GI → CH1 and GII → CH2 (BNC cables, scope inputs 1 MΩ).
//! - `MHO954_HW_BODE=1`: GI through a T piece to CH1 and CH2 (flat response).

use std::time::Duration;

use mho_driver::params::{ParamMap, ParamValue};
use mho_driver::{
    ChannelUpdate, Connection, GeneratorUpdate, Scope, Slope, TimebaseUpdate, TriggerSweep,
    TriggerUpdate, WaveMode,
};

fn scope() -> Option<Scope> {
    if std::env::var_os("MHO954_HW").is_none() {
        eprintln!("skipped: set MHO954_HW=1");
        return None;
    }
    let conn: Connection = std::env::var("MHO954_HW_CONN")
        .unwrap_or_else(|_| "usb".into())
        .parse()
        .expect("MHO954_HW_CONN");
    Some(Scope::open(&conn).expect("open scope"))
}

/// Scope for a generator test, `None` (skipped) unless `flag` is set and the generator is
/// confirmed on this instrument.
fn generator_scope(flag: &str) -> Option<Scope> {
    if std::env::var_os(flag).is_none() {
        eprintln!("skipped: set {flag}=1 (see the cabling in the module docs)");
        return None;
    }
    let mut s = scope()?;
    let caps = s.capabilities().unwrap();
    assert!(
        caps.generator.is_some(),
        "no usable generator: {caps:?} (run `mho954 verify afg` first)"
    );
    Some(s)
}

/// Writes one generator channel the same way the service does (output last).
fn generator(s: &mut Scope, n: u8, u: GeneratorUpdate) {
    let mut settings = u.settings().unwrap();
    if !settings.is_empty() {
        s.group_set("afg", Some(n), &settings, false).unwrap();
    }
    if let Some(on) = u.output {
        settings.clear();
        settings.insert("output".into(), ParamValue::Bool(on));
        s.group_set("afg", Some(n), &settings, false).unwrap();
    }
}

fn outputs_off(s: &mut Scope) {
    for n in 1..=2 {
        let mut off = ParamMap::new();
        off.insert("output".into(), ParamValue::Bool(false));
        s.group_set("afg", Some(n), &off, false).unwrap();
    }
}

fn measured(s: &mut Scope, item: &str, ch: u8) -> f64 {
    let m = s.measure(item, &format!("CHAN{ch}"), None).unwrap();
    m.value
        .unwrap_or_else(|| panic!("{item} on CH{ch} invalid"))
}

fn near(what: &str, got: f64, want: f64, rel: f64) {
    assert!(
        (got / want - 1.0).abs() <= rel,
        "{what}: {got} not within {:.0} % of {want}",
        rel * 100.0
    );
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

#[test]
#[ignore]
fn generator_outputs_on_ch1_and_ch2() {
    let Some(mut s) = generator_scope("MHO954_HW_AFG") else {
        return;
    };
    let before = s.status().unwrap();
    let sine = GeneratorUpdate {
        function: Some("SIN".into()),
        frequency: Some(1e3),
        amplitude: Some(2.0),
        offset: Some(0.0),
        load: Some("highz".into()),
        output: Some(true),
        ..Default::default()
    };
    let square = GeneratorUpdate {
        function: Some("SQU".into()),
        frequency: Some(5e3),
        amplitude: Some(1.0),
        offset: Some(0.0),
        duty: Some(50.0),
        load: Some("highz".into()),
        output: Some(true),
        ..Default::default()
    };
    generator(&mut s, 1, sine);
    generator(&mut s, 2, square);
    healthy(&mut s);
    for (ch, scale) in [(1u8, 0.5), (2, 0.25)] {
        s.set_channel(
            ch,
            &ChannelUpdate {
                display: Some(true),
                scale: Some(scale),
                offset: Some(0.0),
                ..Default::default()
            },
        )
        .unwrap();
    }
    s.set_timebase(&TimebaseUpdate {
        scale: Some(500e-6),
        offset: Some(0.0),
        ..Default::default()
    })
    .unwrap();
    s.set_trigger(&TriggerUpdate {
        sweep: Some(TriggerSweep::Auto),
        edge_source: Some("CHAN1".into()),
        edge_slope: Some(Slope::Positive),
        edge_level: Some(0.0),
        ..Default::default()
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let checks = || {
        near("CH1 FREQ", measured(&mut s, "FREQ", 1), 1e3, 0.01);
        near("CH1 VPP", measured(&mut s, "VPP", 1), 2.0, 0.08);
        near("CH2 FREQ", measured(&mut s, "FREQ", 2), 5e3, 0.01);
        near("CH2 VPP", measured(&mut s, "VPP", 2), 1.0, 0.1);
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(checks));
    outputs_off(&mut s);
    for c in &before.channels {
        s.set_channel(
            c.channel,
            &ChannelUpdate {
                display: Some(c.display),
                scale: Some(c.scale),
                offset: Some(c.offset),
                ..Default::default()
            },
        )
        .unwrap();
    }
    healthy(&mut s);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[ignore]
fn bode_sweep_through_a_t_piece_is_flat() {
    let Some(mut s) = generator_scope("MHO954_HW_BODE") else {
        return;
    };
    let req = mho_driver::BodeRequest {
        start: 1e3,
        stop: 10e6,
        points: 9,
        amplitude: 1.0,
        ..Default::default()
    };
    let pts = s.bode_sweep(&req, &mut |_, _| {}).unwrap();
    for p in &pts {
        assert!(p.gain_db.abs() < 0.5, "{p:?}");
        assert!(p.phase_deg.abs() < 5.0, "{p:?}");
    }
    healthy(&mut s);
}
