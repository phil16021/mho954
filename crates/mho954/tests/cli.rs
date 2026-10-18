//! CLI end-to-end tests against the simulator (`-c sim`, no hardware, no daemon).

use std::path::Path;

use assert_cmd::Command;

/// `mho954` with an isolated config and the simulator.
fn cli(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("mho954").unwrap();
    c.env("MHO954_CONFIG", dir.join("config.toml"))
        .env("MHO954_URL", "http://127.0.0.1:9") // never a running daemon
        .env_remove("MHO954_SIM")
        .args(["-c", "sim"]);
    c
}

fn stdout(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn stderr_of_failure(c: &mut Command) -> String {
    let out = c.output().unwrap();
    assert!(
        !out.status.success(),
        "expected failure, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn idn_status_and_firmware() {
    let d = tempfile::tempdir().unwrap();
    let idn = stdout(cli(d.path()).arg("idn"));
    assert!(idn.contains("MHO954") && idn.contains("00.01.00"), "{idn}");
    let status = stdout(cli(d.path()).arg("status"));
    assert!(
        status.contains("CH1 ON") && status.contains("timebase"),
        "{status}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout(cli(d.path()).args(["status", "--json"]))).unwrap();
    assert_eq!(json["channels"].as_array().unwrap().len(), 4);
    let fw = stdout(cli(d.path()).args(["fw", "status"]));
    assert!(fw.contains("shortened version"), "{fw}");
}

#[test]
fn measurements() {
    let d = tempfile::tempdir().unwrap();
    let list = stdout(cli(d.path()).args(["measure", "--list"]));
    assert!(list.contains("FREQUENCY") && list.contains("RRDELAY"));
    let m = stdout(cli(d.path()).args(["measure", "FREQ", "-s", "2"]));
    assert!(m.contains("5.00") && m.contains("kHz"), "{m}");
    let local = stdout(cli(d.path()).args(["measure", "--local", "-s", "1"]));
    assert!(local.contains("freq 1.000 kHz"), "{local}");
    let nodata = stdout(
        cli(d.path())
            .env("MHO954_SIM", "nosignal=1")
            .args(["measure", "--local", "-s", "1"]),
    );
    assert!(nodata.contains("no data"), "{nodata}");
    let err = stderr_of_failure(cli(d.path()).args(["measure", "BOGUS", "-s", "1"]));
    assert!(err.contains("unknown measurement item"), "{err}");
}

#[test]
fn capture_formats() {
    let d = tempfile::tempdir().unwrap();
    for ext in ["csv", "npy", "json"] {
        let f = d.path().join(format!("w.{ext}"));
        stdout(cli(d.path()).args(["capture", "-s", "2", "-o"]).arg(&f));
        assert!(f.metadata().unwrap().len() > 1000, "{ext} too small");
    }
    assert!(d.path().join("w.json").exists());
    let csv = std::fs::read_to_string(d.path().join("w.csv")).unwrap();
    assert!(csv.starts_with("time_s,CHAN2_V"));
    assert!(
        std::fs::read(d.path().join("w.npy"))
            .unwrap()
            .starts_with(b"\x93NUMPY")
    );
    let err = stderr_of_failure(
        cli(d.path())
            .args(["capture", "-s", "2", "-o"])
            .arg(d.path().join("w.txt")),
    );
    assert!(err.contains("unsupported output extension"), "{err}");
}

#[test]
fn screenshot_fft_options() {
    let d = tempfile::tempdir().unwrap();
    let png = d.path().join("s.png");
    stdout(cli(d.path()).args(["screenshot", "-o"]).arg(&png));
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
    let fft = stdout(cli(d.path()).args(["fft", "-s", "4", "-n", "1"]));
    assert!(fft.contains("200.0 kHz"), "{fft}");
    let opts = stdout(cli(d.path()).arg("options"));
    assert!(opts.contains("BWU05T08") && opts.contains("[ ]"));
    let err = stderr_of_failure(cli(d.path()).args(["options", "install", "not-a-license"]));
    assert!(err.contains("MHO900-<OPTION>@<code>"), "{err}");
}

#[test]
fn backup_and_restore() {
    let d = tempfile::tempdir().unwrap();
    let out = stdout(cli(d.path()).args(["backup", "--dir"]).arg(d.path()));
    let dir = std::fs::read_dir(d.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("backup directory");
    for f in [
        "info.json",
        "status.json",
        "options.json",
        "setup.bin",
        "screenshot.png",
    ] {
        assert!(dir.join(f).exists(), "{f} missing ({out})");
    }
    let restored = stdout(cli(d.path()).arg("restore").arg(dir.join("setup.bin")));
    assert!(restored.contains("setup restored"));
}

#[test]
fn settings_and_network() {
    let d = tempfile::tempdir().unwrap();
    let ch =
        stdout(cli(d.path()).args(["channel", "3", "--on", "--scale", "0.5", "--coupling", "ac"]));
    assert!(
        ch.contains("CH3 ON") && ch.contains("500.0 mV/div") && ch.contains("AC"),
        "{ch}"
    );
    let tb = stdout(cli(d.path()).args(["timebase", "--scale", "1e-3"]));
    assert!(tb.contains("1.000 ms/div"), "{tb}");
    let tr = stdout(cli(d.path()).args(["trigger", "--slope", "falling", "--level", "-0.5"]));
    assert!(tr.contains("NEG") && tr.contains("-500.0 mV"), "{tr}");
    let net = stdout(cli(d.path()).args([
        "net",
        "set",
        "--static",
        "--ip",
        "192.168.1.50",
        "--mask",
        "255.255.255.0",
    ]));
    assert!(
        net.contains("192.168.1.50") && net.contains("static"),
        "{net}"
    );
    let err = stderr_of_failure(cli(d.path()).args(["net", "set", "--ip", "999.1.1.1"]));
    assert!(err.contains("invalid IPv4"), "{err}");
    let err = stderr_of_failure(cli(d.path()).args(["net", "set"]));
    assert!(err.contains("nothing to change"), "{err}");
}

#[test]
fn raw_scpi() {
    let d = tempfile::tempdir().unwrap();
    let r = stdout(cli(d.path()).args(["scpi", ":TIM:SCAL?"]));
    assert!(r.contains("E-4"), "{r}");
    let r = stdout(cli(d.path()).args(["scpi", ":SYST:SET?"]));
    assert!(r.contains("binary block"), "{r}");
    let err = stderr_of_failure(cli(d.path()).args(["scpi", ":WLAN:SSID?"]));
    assert!(err.contains("no answer"), "{err}");
}

#[test]
fn config_handling() {
    let d = tempfile::tempdir().unwrap();
    let toml = stdout(cli(d.path()).arg("config"));
    assert!(
        toml.contains("connection = \"sim\"") && toml.contains("[live]"),
        "{toml}"
    );
    std::fs::write(d.path().join("config.toml"), "conection = \"usb\"\n").unwrap();
    let err = stderr_of_failure(cli(d.path()).arg("config"));
    assert!(err.contains("unknown field"), "{err}");
    std::fs::write(d.path().join("config.toml"), "bind = \"0.0.0.0:8954\"\n").unwrap();
    let err = stderr_of_failure(cli(d.path()).arg("serve"));
    assert!(err.contains("api_token"), "{err}");
}

#[test]
fn mcp_over_stdio() {
    let d = tempfile::tempdir().unwrap();
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"identify","arguments":{}}}"#,
    ]
    .join("\n")
        + "\n";
    let out = cli(d.path())
        .arg("mcp")
        .write_stdin(input)
        .timeout(std::time::Duration::from_secs(20))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(r#""id":2"#) && text.contains("MHO954"),
        "{text}"
    );
}

#[test]
fn decoder_needs_setup() {
    let d = tempfile::tempdir().unwrap();
    // Each CLI call starts a fresh simulator, so the decoder is off.
    let err = stderr_of_failure(cli(d.path()).args(["decode", "1"]));
    assert!(err.contains("decoder 1 is off"), "{err}");
    let groups = stdout(cli(d.path()).args(["groups", "bus", "--params"]));
    assert!(
        groups.contains("bus.rs232") && groups.contains("tx_threshold"),
        "{groups}"
    );
}

#[test]
fn readouts_and_commands() {
    let d = tempfile::tempdir().unwrap();
    let list = stdout(cli(d.path()).args(["do", "--list"]));
    assert!(
        list.contains("save.setup") && list.contains("needs --yes"),
        "{list}"
    );
    let reads = stdout(cli(d.path()).args(["read", "--list"]));
    assert!(reads.contains("cursor.manual"), "{reads}");
    let done = stdout(cli(d.path()).args(["do", "save.setup", "D:/bench.stp"]));
    assert!(done.contains("done"), "{done}");
    let err = stderr_of_failure(cli(d.path()).args(["do", "reset"]));
    assert!(err.contains("confirmed"), "{err}");
    let err = stderr_of_failure(cli(d.path()).args(["do", "save.setup", "../x.stp"]));
    assert!(err.contains("C:/"), "{err}");
    assert!(stdout(cli(d.path()).args(["do", "reset", "--yes"])).contains("done"));
}

#[test]
fn logic_and_bode() {
    let d = tempfile::tempdir().unwrap();
    let vcd = d.path().join("cap.vcd");
    let out = stdout(cli(d.path()).args(["logic", "--bus", "3", "-o"]).arg(&vcd));
    assert!(out.contains("5 changes"), "{out}");
    assert!(
        std::fs::read_to_string(&vcd)
            .unwrap()
            .contains("$var wire 1")
    );
    let csv = d.path().join("bode.csv");
    let out = stdout(
        cli(d.path())
            .args([
                "bode", "--start", "1k", "--stop", "100k", "--points", "3", "-o",
            ])
            .arg(&csv),
    );
    assert!(out.contains("3 points"), "{out}");
    let text = std::fs::read_to_string(&csv).unwrap();
    // simulated low-pass: corner at 10 kHz, summary first
    assert!(text.starts_with("# corner_3db_hz: "), "{text}");
    assert!(
        text.contains("frequency_hz,gain_db,phase_deg,phase_unwrapped_deg\n1000,"),
        "{text}"
    );
    assert!(out.contains("-3 dB at"), "{out}");
    // scope-native Bode plot: screenshot
    let png = d.path().join("bode.png");
    let out = stdout(
        cli(d.path())
            .args(["bode", "--native", "--start", "100", "--stop", "100k", "-o"])
            .arg(&png),
    );
    assert!(out.contains("screenshot written"), "{out}");
    assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn generator_and_capabilities() {
    let d = tempfile::tempdir().unwrap();
    let caps = stdout(cli(d.path()).arg("caps"));
    assert!(caps.contains("800 MHz") && caps.contains("500M"), "{caps}");
    assert!(caps.contains("FlexRay"), "{caps}");
    let out = stdout(cli(d.path()).args([
        "afg", "2", "--func", "squ", "--freq", "1k", "--ampl", "1", "--duty", "25", "--on",
    ]));
    assert!(
        out.contains("SQU") && out.contains("output         true"),
        "{out}"
    );
    let err = stderr_of_failure(cli(d.path()).args(["afg", "1", "--func", "ramp", "--freq", "5M"]));
    assert!(err.contains("RAMP limit"), "{err}");
    let opts = stdout(cli(d.path()).arg("options"));
    assert!(opts.contains("[s] EMBD"), "{opts}");
}
