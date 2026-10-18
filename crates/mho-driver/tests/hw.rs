//! Hardware tests against a real MHO954 (USB unless `MHO954_HW_CONN` names another
//! connection, e.g. `tcp:192.168.1.109`). Run with
//! `MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1`.
//!
//! Only verified commands are used, and every step is followed by `*IDN?` so a problem is
//! caught immediately (the firmware hangs after ~5 unknown queries per power cycle).
//!
//! Generator tests drive real signals and need the bench cabling:
//! - `MHO954_HW_AFG=1`: GI → CH1 and GII → CH2 (BNC cables, scope inputs 1 MΩ). The FFT
//!   peak test uses only GI, on the input named by `MHO954_HW_FFT_CH` (default 1).
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

fn math_fft(s: &mut Scope, values: &[(&str, ParamValue)], select: bool) {
    let m: ParamMap = values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    s.group_set("math.fft", Some(1), &m, select).unwrap();
}

/// Raw peak lines and their parse, printed for the documentation.
fn peak_table(s: &mut Scope, what: &str) -> (Vec<String>, Vec<mho_dsp::ScopePeak>) {
    std::thread::sleep(Duration::from_millis(1500));
    let lines = s.scpi().query_lines(":MATH1:FFT:SEAR:RES?").unwrap();
    eprintln!("--- {what}: {} line(s)", lines.len());
    for l in &lines {
        eprintln!("{l:?}");
    }
    let peaks = s.fft_peaks(1).unwrap();
    eprintln!("parsed: {peaks:?}");
    healthy(s);
    (lines, peaks)
}

#[test]
#[ignore]
fn fft_peak_table_from_square() {
    let Some(mut s) = generator_scope("MHO954_HW_AFG") else {
        return;
    };
    // GI goes to this input (`MHO954_HW_FFT_CH`, default 1).
    let ch: u8 = std::env::var("MHO954_HW_FFT_CH")
        .map(|v| v.parse().expect("MHO954_HW_FFT_CH"))
        .unwrap_or(1);
    let src = format!("CHAN{ch}");
    let before = s.status().unwrap();
    let math = s.group_get("math", Some(1), false).unwrap();
    let fft = s.group_get("math.fft", Some(1), false).unwrap();
    // 1 Vpp square: odd harmonics only, the n-th at 4/(nπ) · 0.5 V peak.
    generator(
        &mut s,
        1,
        GeneratorUpdate {
            function: Some("SQU".into()),
            frequency: Some(1e3),
            amplitude: Some(1.0),
            offset: Some(0.0),
            duty: Some(50.0),
            load: Some("highz".into()),
            output: Some(true),
            ..Default::default()
        },
    );
    s.set_channel(
        ch,
        &ChannelUpdate {
            display: Some(true),
            probe: Some(1.0),
            scale: Some(0.25),
            offset: Some(0.0),
            ..Default::default()
        },
    )
    .unwrap();
    s.set_timebase(&TimebaseUpdate {
        scale: Some(1e-3),
        offset: Some(0.0),
        ..Default::default()
    })
    .unwrap();
    s.set_trigger(&TriggerUpdate {
        sweep: Some(TriggerSweep::Auto),
        edge_source: Some(src.clone()),
        edge_slope: Some(Slope::Positive),
        edge_level: Some(0.0),
        ..Default::default()
    })
    .unwrap();
    s.action(mho_driver::Action::Run).unwrap();
    let text = |t: &str| ParamValue::Text(t.into());
    math_fft(
        &mut s,
        &[
            ("source", text(&src)),
            ("window", text("FLATtop")),
            ("unit", text("DB")),
            ("mode", text("NORMal")),
            ("span", ParamValue::Real(20e3)),
            ("center", ParamValue::Real(10e3)),
            ("peak_search", ParamValue::Bool(true)),
            ("peaks", ParamValue::Int(5)),
            ("peak_order", text("AMPorder")),
            // In the unit's scale (dBV here); the factory value 5.5 hides a 1 Vpp signal.
            ("peak_threshold", ParamValue::Real(-60.0)),
            ("peak_excursion", ParamValue::Real(3.0)),
        ],
        true,
    );
    let checks = || {
        std::thread::sleep(Duration::from_millis(800));
        near("FREQ", measured(&mut s, "FREQ", ch), 1e3, 0.01);
        // VPP includes the edge overshoot (up to 1.19 V seen); top − base does not.
        near("VAMP", measured(&mut s, "VAMP", ch), 1.0, 0.1);
        let (lines, peaks) = peak_table(&mut s, "DB, AMPorder, 5 peaks");
        assert_eq!(peaks.len(), lines.len(), "unparsed lines");
        assert_eq!(peaks.len(), 5);
        for (i, p) in peaks.iter().enumerate() {
            assert_eq!(p.rank as usize, i + 1);
        }
        let mut f: Vec<f64> = peaks.iter().map(|p| p.frequency).collect();
        f.sort_by(f64::total_cmp);
        // Peaks lie on the scope's bin grid (48.8 Hz at a 20 kHz span); the flat top of the
        // window puts the reported bin up to ~1.5 bins off (1 kHz showed as 1.025 kHz,
        // 3 kHz as 3.075 kHz).
        for (got, want) in f.iter().zip([1e3, 3e3, 5e3, 7e3, 9e3]) {
            assert!(
                (got - want).abs() < 125.0,
                "harmonic {got} Hz, expected {want} Hz"
            );
        }
        let d = peaks[0].amplitude - peaks[1].amplitude;
        assert!((d - 9.54).abs() < 1.0, "fundamental − 3rd = {d} dB");
        eprintln!(
            "fundamental {} (rms reference -6.93 dBV, peak reference -3.92 dBV)",
            peaks[0].amplitude_text
        );

        // Threshold and excursion are in the unit's scale; the scope converts them when the
        // unit changes, so they are written after it (table order).
        let db = |s: &mut Scope, window: &str| {
            math_fft(
                s,
                &[
                    ("window", text(window)),
                    ("unit", text("DB")),
                    ("peak_threshold", ParamValue::Real(-60.0)),
                    ("peak_excursion", ParamValue::Real(3.0)),
                ],
                false,
            );
        };
        // VRMS vs dB of the same peak: the ratio shows whether VRMS is window-corrected.
        for (window, gain) in [("FLATtop", 0.2156), ("HANNing", 0.5), ("RECTangle", 1.0)] {
            db(&mut s, window);
            let (_, in_db) = peak_table(&mut s, &format!("{window}, DB"));
            math_fft(
                &mut s,
                &[
                    ("unit", text("VRMS")),
                    ("peak_threshold", ParamValue::Real(1e-3)),
                    ("peak_excursion", ParamValue::Real(1e-3)),
                ],
                false,
            );
            let (lines, in_v) = peak_table(&mut s, &format!("{window}, VRMS"));
            assert_eq!(in_v.len(), lines.len(), "unparsed lines");
            let ratio = in_v[0].amplitude / 10f64.powf(in_db[0].amplitude / 20.0);
            eprintln!("{window}: VRMS / dB = {ratio:.4} (coherent gain {gain})");
            if window == "FLATtop" {
                near("flat-top VRMS / dB", ratio, gain, 0.05);
            }
        }

        db(&mut s, "FLATtop");
        math_fft(&mut s, &[("peak_order", text("FREQorder"))], false);
        let (lines, peaks) = peak_table(&mut s, "DB, FREQorder");
        assert_eq!(peaks.len(), lines.len(), "unparsed lines");
        assert!(peaks.windows(2).all(|w| w[0].frequency < w[1].frequency));

        math_fft(
            &mut s,
            &[
                ("peak_order", text("AMPorder")),
                ("peaks", ParamValue::Int(15)),
            ],
            false,
        );
        let (lines, peaks) = peak_table(&mut s, "DB, AMPorder, 15 peaks");
        assert_eq!(peaks.len(), lines.len(), "unparsed lines");
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(checks));
    outputs_off(&mut s);
    s.group_set("math.fft", Some(1), &fft, false).unwrap();
    s.group_set("math", Some(1), &math, false).unwrap();
    let c = &before.channels[ch as usize - 1];
    s.set_channel(
        ch,
        &ChannelUpdate {
            display: Some(c.display),
            probe: Some(c.probe),
            scale: Some(c.scale),
            offset: Some(c.offset),
            ..Default::default()
        },
    )
    .unwrap();
    s.set_timebase(&TimebaseUpdate {
        scale: Some(before.timebase.scale),
        offset: Some(before.timebase.offset),
        ..Default::default()
    })
    .unwrap();
    let t = &before.trigger;
    s.set_trigger(&TriggerUpdate {
        mode: Some(t.mode.clone()),
        sweep: Some(t.sweep),
        edge_source: Some(t.edge_source.clone()),
        edge_slope: Some(t.edge_slope),
        edge_level: Some(t.edge_level),
        ..Default::default()
    })
    .unwrap();
    if before.trigger_status.eq_ignore_ascii_case("STOP") {
        s.action(mho_driver::Action::Stop).unwrap();
    }
    healthy(&mut s);
    if let Err(e) = r {
        std::panic::resume_unwind(e);
    }
}
