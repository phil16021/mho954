//! Driver features against the simulator: verification gate, decoders, keyed thresholds.

use mho_driver::params::{ParamMap, ParamValue};
use mho_driver::{Connection, DriverError, Scope, SimTransport};

#[test]
fn strict_simulator_refuses_unverified_queries() {
    // `from_transport` applies the hardware gate, like a real instrument.
    let mut s = Scope::from_transport(Box::new(SimTransport::new())).unwrap();
    assert!(!s.trusts_unverified());
    assert!(matches!(s.bus_data(1), Err(DriverError::Unsupported(_))));
    assert!(matches!(s.dvm_value(), Err(DriverError::Unsupported(_))));
    assert!(matches!(
        s.group_get("bus.spi", Some(1), false),
        Err(DriverError::Unsupported(_))
    ));
    // Writing is always allowed (errors land in the error queue).
    let mut v = ParamMap::new();
    v.insert("baud".into(), ParamValue::Int(115_200));
    assert_eq!(s.group_set("bus.rs232", Some(2), &v, true).unwrap(), None);
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
