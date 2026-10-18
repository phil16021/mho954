//! Driver features against the simulator: verification gate, decoders, keyed thresholds.

use mho_driver::params::{ParamMap, ParamValue};
use mho_driver::{Connection, DriverError, Scope, SimTransport};

#[test]
fn strict_simulator_refuses_unverified_queries() {
    // `from_transport` applies the hardware gate, like a real instrument.
    let mut s = Scope::from_transport(Box::new(SimTransport::new())).unwrap();
    assert!(!s.trusts_unverified());
    // Still unverified on the test scope: everything of the generator.
    assert!(matches!(
        s.group_get("afg", Some(1), false),
        Err(DriverError::Unsupported(_))
    ));
    // Verified on the instrument: allowed under the gate too.
    assert!(s.group_get("bus.spi", Some(1), false).is_ok());
    // Without the AFG option the sweep is refused before anything is sent to the generator.
    let mut noafg = Scope::from_transport(Box::new(SimTransport::with_options(
        mho_driver::SimOptions {
            afg: false,
            ..Default::default()
        },
    )))
    .unwrap();
    assert!(matches!(
        noafg.bode_sweep(&Default::default(), &mut |_, _| {}),
        Err(DriverError::Unsupported(_))
    ));
    // Writing is always allowed (errors land in the error queue); no read-back unverified.
    let mut v = ParamMap::new();
    v.insert("frequency".into(), ParamValue::Real(1e3));
    assert_eq!(s.group_set("afg", Some(1), &v, true).unwrap(), None);
}

#[test]
fn decoder_setup_and_event_table() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    assert!(s.trusts_unverified());
    // Off: refused before the (expensive if unanswered) data query.
    assert!(matches!(s.bus_data(2), Err(DriverError::State(_))));
    assert!(matches!(s.bus_data(5), Err(DriverError::Invalid(_))));

    let mut v = ParamMap::new();
    v.insert("baud".into(), ParamValue::Int(115_200));
    v.insert("tx".into(), ParamValue::Text("CHAN2".into()));
    v.insert("tx_threshold".into(), ParamValue::Real(1.25));
    let state = s
        .group_set("bus.rs232", Some(2), &v, true)
        .unwrap()
        .unwrap();
    assert_eq!(state["baud"], ParamValue::Int(115_200));
    assert_eq!(state["tx"], ParamValue::Text("CHAN2".into()));
    assert_eq!(state["tx_threshold"], ParamValue::Real(1.25));
    // Instance 1 untouched.
    let b1 = s.group_get("bus", Some(1), false).unwrap();
    assert_eq!(b1["display"], ParamValue::Bool(false));
    let b2 = s.group_get("bus", Some(2), false).unwrap();
    assert_eq!(b2["display"], ParamValue::Bool(true));
    assert_eq!(b2["mode"], ParamValue::Text("RS232".into()));

    let mut f = ParamMap::new();
    f.insert("format".into(), ParamValue::Text("ascii".into()));
    s.group_set("bus", Some(2), &f, false).unwrap();
    let t = s.bus_data(2).unwrap();
    assert_eq!(t.protocol, "RS232");
    assert_eq!(t.columns, ["Time", "TX", "RX"]);
    let text: String = t.rows.iter().map(|r| r.cells[1].as_str()).collect();
    assert_eq!(text, "Hello");
    assert!((t.rows[0].time.unwrap() + 200e-6).abs() < 1e-12);
    // The session stays in sync after the block-plus-text answer.
    assert_eq!(s.idn().model, "MHO954");
    assert!(s.scpi().query("*IDN?").unwrap().contains("MHO954"));
}

#[test]
fn math_channel_and_fft_peaks() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    assert!(s.displayed_math().unwrap().is_empty());
    let mut v = ParamMap::new();
    v.insert("operator".into(), ParamValue::Text("SUBTract".into()));
    v.insert("source1".into(), ParamValue::Text("CHAN2".into()));
    v.insert("source2".into(), ParamValue::Text("CHAN2".into()));
    v.insert("scale".into(), ParamValue::Real(0.5));
    let state = s.group_set("math", Some(2), &v, true).unwrap().unwrap();
    assert_eq!(state["operator"], ParamValue::Text("SUBT".into()));
    assert_eq!(state["display"], ParamValue::Bool(true));
    assert_eq!(s.displayed_math().unwrap(), [2]);
    // CH2 - CH2 = 0 (noise is drawn per operand, so allow a little)
    let w = s.screen_waveform("MATH2", None).unwrap();
    let a = w.analyze();
    assert!(a.vpp < 0.2, "{a:?}");
    assert!(matches!(
        s.raw_waveform("MATH2", &mut |_, _| {}),
        Err(DriverError::Invalid(_))
    ));

    // FFT with peak search: refused while off, then CH2's 5 kHz sine is the top peak
    assert!(matches!(s.fft_peaks(1), Err(DriverError::State(_))));
    let mut f = ParamMap::new();
    f.insert("source".into(), ParamValue::Text("CHANnel2".into()));
    f.insert("peak_search".into(), ParamValue::Bool(true));
    f.insert("peaks".into(), ParamValue::Int(3));
    s.group_set("math.fft", Some(1), &f, true).unwrap();
    let p = s.fft_peaks(1).unwrap();
    assert_eq!(p.len(), 3, "{p:?}");
    assert!((p[0].frequency - 5e3).abs() < 200.0, "{p:?}");
    // FFT results are not time-domain traces
    assert_eq!(s.displayed_math().unwrap(), [2]);
    assert!(s.scpi().query("*IDN?").unwrap().contains("MHO954"));
}

#[test]
fn readouts_need_their_mode_and_commands_validate() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    // cursor off → refused before any value query
    assert!(matches!(
        s.readout("cursor.manual"),
        Err(DriverError::State(_))
    ));
    s.group_set("cursor.manual", None, &ParamMap::new(), true)
        .unwrap();
    let v = s.readout("cursor.manual").unwrap();
    assert_eq!(v.len(), 7);
    assert!(matches!(v["inv_dx"], ParamValue::Real(_)));
    let mut on = ParamMap::new();
    on.insert("enable".into(), ParamValue::Bool(true));
    s.group_set("histogram", None, &on, false).unwrap();
    let h = s.readout("histogram").unwrap();
    assert_eq!(h["statistics.bin_width"], ParamValue::Text("20ns".into()));
    assert_eq!(s.readout("save").unwrap()["done"], ParamValue::Int(1));
    assert!(matches!(s.readout("nope"), Err(DriverError::Invalid(_))));

    s.run_command("save.setup", Some("D:/bench1.stp"), false)
        .unwrap();
    s.run_command("mask.reset", None, false).unwrap();
    for (id, arg) in [
        ("save.setup", None),
        ("save.setup", Some("E:/x.stp")),
        ("save.setup", Some("D:/x.png")),
        ("mask.reset", Some("D:/x.pf")),
        ("reset", None),
        ("nope", None),
    ] {
        assert!(
            matches!(s.run_command(id, arg, false), Err(DriverError::Invalid(_))),
            "{id} {arg:?}"
        );
    }
    // confirmed reset restores the defaults
    let mut ch = ParamMap::new();
    ch.insert("label".into(), ParamValue::Text("probe".into()));
    s.group_set("channel", Some(2), &ch, false).unwrap();
    s.run_command("reset", None, true).unwrap();
    let c2 = s.group_get("channel", Some(2), false).unwrap();
    assert_ne!(c2["label"], ParamValue::Text("probe".into()));
    assert!(s.scpi().query("*IDN?").unwrap().contains("MHO954"));
}

#[test]
fn logic_capture_without_probe_restores_the_decoder() {
    let opts = mho_driver::SimOptions {
        la_probe: false,
        ..Default::default()
    };
    // `from_transport` applies the gate; the decoder queries are verified on the instrument.
    let mut s = Scope::from_transport(Box::new(SimTransport::with_options(opts))).unwrap();
    let err = s.logic_capture(1).unwrap_err();
    assert!(matches!(err, DriverError::Unsupported(_)), "{err}");
    assert!(err.to_string().contains("logic probe"), "{err}");
    let bus = s.group_get("bus", Some(1), false).unwrap();
    assert_eq!(bus.get("display"), Some(&ParamValue::Bool(false)));
    let par = s.group_get("bus.parallel", Some(1), false).unwrap();
    assert_ne!(par.get("bus"), Some(&ParamValue::Text("D15D0".into())));
}

#[test]
fn logic_capture_via_parallel_decoder() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    assert!(matches!(s.logic_capture(0), Err(DriverError::Invalid(_))));
    let cap = s.logic_capture(4).unwrap();
    assert_eq!(cap.lines.len(), 16);
    let values: Vec<u64> = cap.events.iter().map(|e| e.value).collect();
    assert_eq!(
        values,
        b"Hello".iter().map(|&b| u64::from(b)).collect::<Vec<_>>()
    );
    let vcd = mho_dsp::write_vcd(&cap.events, &cap.lines);
    assert!(vcd.contains("$var wire 1 0 D15 $end"), "{vcd}");
}

#[test]
fn bode_sweep_of_simulated_low_pass() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    let before = s.timebase().unwrap();
    let req = mho_driver::BodeRequest {
        start: 1e3,
        stop: 100e3,
        points: 5,
        settle_ms: 0,
        ..Default::default()
    };
    let mut calls = 0;
    let pts = s.bode_sweep(&req, &mut |_, _| calls += 1).unwrap();
    assert_eq!((pts.len(), calls), (5, 5));
    // first-order low-pass, corner 10 kHz (point 3 of 1k..100k log-spaced)
    let corner = &pts[2];
    assert!((corner.frequency - 10e3).abs() < 1.0);
    assert!((corner.gain_db + 3.01).abs() < 0.3, "{pts:?}");
    assert!((corner.phase_deg + 45.0).abs() < 3.0, "{pts:?}");
    assert!(
        pts[0].gain_db.abs() < 0.3 && pts[4].gain_db < -19.0,
        "{pts:?}"
    );
    // timebase restored, generator off
    assert_eq!(s.timebase().unwrap().scale, before.scale);
    let same_channel = mho_driver::BodeRequest {
        output: 1,
        ..Default::default()
    };
    assert!(matches!(
        s.bode_sweep(&same_channel, &mut |_, _| {}),
        Err(DriverError::Invalid(_))
    ));
}

fn sim(opts: mho_driver::SimOptions) -> Scope {
    // `from_transport` applies the hardware gate; tests that read unverified groups use
    // `Scope::open(&Connection::Sim)` instead.
    Scope::from_transport(Box::new(SimTransport::with_options(opts))).unwrap()
}

#[test]
fn capabilities_follow_options_and_modules() {
    // Default simulator = the upgraded test scope.
    let c = sim(Default::default()).capabilities().unwrap();
    assert_eq!(c.bandwidth_hz, Some(800e6));
    assert_eq!(c.memory_limit(1), 500_000_000);
    assert_eq!(c.generator.as_ref().unwrap().max_sine_hz, 100e6);
    assert_eq!(c.protocols.len(), 4);
    // Before the upgrade.
    let mut plain = sim(mho_driver::SimOptions {
        licensed: false,
        ..Default::default()
    });
    let c = plain.capabilities().unwrap();
    assert_eq!(
        (c.bandwidth_hz, c.memory_limit(1)),
        (Some(500e6), 100_000_000)
    );
    assert!(c.protocols.is_empty());
    // Option groups are refused without their license, for reading and writing.
    let mut v = ParamMap::new();
    v.insert("baud".into(), ParamValue::Text("10000000".into()));
    for r in [
        plain.group_get("bus.flexray", Some(1), true).map(|_| ()),
        plain
            .group_set("bus.flexray", Some(1), &v, true)
            .map(|_| ()),
    ] {
        assert!(matches!(r, Err(DriverError::Unsupported(m)) if m.contains("FlexA")));
    }
    // DG field 0 and no AFG license: no generator, its commands are refused too.
    let mut noafg = sim(mho_driver::SimOptions {
        afg: false,
        ..Default::default()
    });
    assert!(noafg.capabilities().unwrap().generator.is_none());
    assert!(matches!(
        noafg.run_command("afg.align_phase", None, false),
        Err(DriverError::Unsupported(_))
    ));
}

#[test]
fn generator_limits_and_outputs() {
    let mut s = Scope::open(&Connection::Sim).unwrap();
    let set = |s: &mut Scope, n: u8, kv: &[(&str, ParamValue)]| {
        let m: ParamMap = kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        s.group_set("afg", Some(n), &m, false)
    };
    use ParamValue::*;
    // Square wave limit (20 MHz with AFG100) comes from the current function as well.
    set(&mut s, 1, &[("function", Text("SQU".into()))]).unwrap();
    assert!(matches!(
        set(&mut s, 1, &[("frequency", Real(30e6))]),
        Err(DriverError::Invalid(m)) if m.contains("SQU")
    ));
    // GI 1 kHz sine 2 Vpp on CH1, GII 5 kHz square on CH2 (the bench cabling).
    set(
        &mut s,
        1,
        &[
            ("function", Text("SIN".into())),
            ("frequency", Real(1e3)),
            ("amplitude", Real(2.0)),
            ("output", Bool(true)),
        ],
    )
    .unwrap();
    set(
        &mut s,
        2,
        &[
            ("function", Text("SQU".into())),
            ("frequency", Real(5e3)),
            ("amplitude", Real(1.0)),
            ("offset", Real(0.5)),
            ("output", Bool(true)),
        ],
    )
    .unwrap();
    s.set_timebase(&mho_driver::TimebaseUpdate {
        scale: Some(200e-6),
        offset: Some(0.0),
        ..Default::default()
    })
    .unwrap();
    let range = |s: &mut Scope, src: &str| {
        let v = s.screen_waveform(src, None).unwrap().volts();
        let max = v.iter().cloned().fold(f64::MIN, f64::max);
        let min = v.iter().cloned().fold(f64::MAX, f64::min);
        (min, max)
    };
    let (lo, hi) = range(&mut s, "CHAN1");
    assert!((hi - lo - 2.0).abs() < 0.15, "GI {lo}..{hi}");
    let (lo, hi) = range(&mut s, "CHAN2");
    assert!(
        lo > -0.1 && (hi - 1.0).abs() < 0.1,
        "GII square 0..1 V: {lo}..{hi}"
    );
    let st = s.group_get("afg", Some(2), false).unwrap();
    assert_eq!(st["period"], ParamValue::Real(2e-4));
}

#[test]
fn memory_depth_limited_by_channels_and_rlu05() {
    let depth = |s: &mut Scope, d: &str| {
        s.set_acquire(&mho_driver::AcquireUpdate {
            memory_depth: Some(d.into()),
            ..Default::default()
        })
    };
    // CH1 and CH2 on: 250M with RLU05.
    let mut s = sim(Default::default());
    assert!(depth(&mut s, "250M").is_ok());
    assert!(matches!(depth(&mut s, "500M"), Err(DriverError::Invalid(m)) if m.contains("250M")));
    assert!(depth(&mut s, "AUTO").is_ok());
    // Without RLU05: 50M with two channels.
    let mut plain = sim(mho_driver::SimOptions {
        licensed: false,
        ..Default::default()
    });
    assert!(depth(&mut plain, "50M").is_ok());
    assert!(depth(&mut plain, "100000000").is_err());
}
