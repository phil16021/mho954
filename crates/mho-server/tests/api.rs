//! End-to-end: server on an ephemeral port with the simulator, exercised through the
//! HTTP client backend, the live WebSocket and the MCP endpoint.

use std::time::Duration;

use mho_driver::{Action, ChannelUpdate, Connection, WaveMode};
use mho_server::ServerOptions;
use mho_service::{
    LiveConfig, LiveEvent, LocalBackend, MeasureRequest, RemoteBackend, ScopeApi, ScpiRequest,
};
use tokio_util::sync::CancellationToken;

async fn start(allow_raw: bool) -> (String, CancellationToken) {
    start_with(allow_raw, None).await
}

async fn start_with(allow_raw: bool, api_token: Option<&str>) -> (String, CancellationToken) {
    let backend = LocalBackend::start(Connection::Sim, allow_raw, LiveConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let opts = ServerOptions {
        bind: addr,
        capture_dir: std::env::temp_dir().join("mho954-test-captures"),
        api_token: api_token.map(str::to_string),
    };
    let shutdown = CancellationToken::new();
    let app = mho_server::router(backend, &opts, shutdown.clone());
    let s = shutdown.clone();
    tokio::spawn(async move {
        mho_server::axum_reexport::serve(listener, app)
            .with_graceful_shutdown(s.cancelled_owned())
            .await
            .unwrap();
    });
    (format!("http://{addr}"), shutdown)
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_roundtrip() {
    let (url, shutdown) = start(true).await;
    let api = RemoteBackend::new(&url);

    let info = api.info().await.unwrap();
    assert!(info.connected);
    assert_eq!(info.idn.unwrap().model, "MHO954");

    let st = api.status().await.unwrap();
    assert_eq!(st.channels.len(), 4);

    let ch = api
        .set_channel(
            3,
            ChannelUpdate {
                display: Some(true),
                scale: Some(0.5),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(ch.display);
    assert_eq!(ch.scale, 0.5);

    let err = api
        .set_channel(9, ChannelUpdate::default())
        .await
        .unwrap_err();
    assert_eq!(err.kind, mho_service::ErrorKind::InvalidArgument);

    let m = api
        .measure(MeasureRequest {
            items: vec!["FREQ".into()],
            source: "1".into(),
            source2: None,
        })
        .await
        .unwrap();
    assert!((m[0].value.unwrap() - 1000.0).abs() < 5.0, "{m:?}");

    let w = api
        .waveform("CHAN2".into(), WaveMode::Normal)
        .await
        .unwrap();
    assert_eq!(w.raw.len(), 1000);

    api.action(Action::Stop).await.unwrap();
    let raw = api.waveform("CHAN1".into(), WaveMode::Raw).await.unwrap();
    assert_eq!(raw.raw.len(), 10_000);
    api.action(Action::Run).await.unwrap();

    let png = api.screenshot().await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));

    let r = api
        .scpi(ScpiRequest {
            command: ":TIM:SCAL?".into(),
            timeout_ms: None,
        })
        .await
        .unwrap();
    assert!(r.response.unwrap().contains("E-4"));

    let net = api.network().await.unwrap();
    assert_eq!(net.status, "UNLINK");
    let net = api
        .set_network(mho_driver::NetworkUpdate {
            mode: Some(mho_driver::IpMode::Static),
            ip: Some("192.168.1.50".into()),
            mask: Some("255.255.255.0".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(net.ip, "192.168.1.50");
    let err = api
        .set_network(mho_driver::NetworkUpdate {
            gateway: Some("1.2.3".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind, mho_service::ErrorKind::InvalidArgument);

    let setup = api.setup_save().await.unwrap();
    api.setup_load(setup).await.unwrap();

    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_scpi_forbidden_by_default() {
    let (url, shutdown) = start(false).await;
    let api = RemoteBackend::new(&url);
    let err = api
        .scpi(ScpiRequest {
            command: "*IDN?".into(),
            timeout_ms: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind, mho_service::ErrorKind::Forbidden);
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_stream_delivers_frames_and_measurements() {
    let (url, shutdown) = start(false).await;
    let api = RemoteBackend::new(&url);
    let mut rx = api.stream().await.unwrap();
    let (mut frames, mut meas) = (0, 0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while (frames < 3 || meas < 1) && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(LiveEvent::Frame(f))) => {
                assert_eq!(f.traces.len(), 2, "CH1 and CH2 are displayed by default");
                frames += 1;
            }
            Ok(Some(LiveEvent::Measurements(m))) => {
                assert!(!m.values.is_empty());
                meas += 1;
            }
            Ok(Some(LiveEvent::Error(e))) => panic!("live error: {e}"),
            _ => break,
        }
    }
    assert!(
        frames >= 3 && meas >= 1,
        "frames {frames}, measurements {meas}"
    );
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_initialize_and_call_tool() {
    let (url, shutdown) = start(false).await;
    let client = reqwest::Client::new();
    let post = |body: serde_json::Value, sid: Option<String>| {
        let mut rb = client
            .post(format!("{url}/mcp"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(s) = sid {
            rb = rb.header("mcp-session-id", s);
        }
        rb.send()
    };
    let init = post(
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
        None,
    )
    .await
    .unwrap();
    assert!(init.status().is_success());
    let sid = init
        .headers()
        .get("mcp-session-id")
        .map(|v| v.to_str().unwrap().to_string());
    let body = init.text().await.unwrap();
    assert!(body.contains("\"name\":\"mho954\""), "{body}");

    post(
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        sid.clone(),
    )
    .await
    .unwrap();
    let list = post(
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        sid.clone(),
    )
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    for tool in [
        "get_status",
        "measure",
        "get_waveform",
        "screenshot",
        "fft_peaks",
    ] {
        assert!(
            list.contains(&format!("\"name\":\"{tool}\"")),
            "missing {tool}"
        );
    }
    let call = post(
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"analyze_waveform","arguments":{"source":"CHAN2"}}}),
        sid,
    )
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
    assert!(
        call.contains("frequency") && call.contains("\"isError\":false"),
        "{call}"
    );
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_requests_are_refused() {
    let (url, shutdown) = start(true).await;
    let client = reqwest::Client::new();
    // DNS rebinding: a foreign Host name pointing at 127.0.0.1.
    let r = client
        .get(format!("{url}/api/v1/info"))
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Cross-site request from a web page.
    let r = client
        .put(format!("{url}/api/v1/channels/1"))
        .header("origin", "https://evil.example")
        .json(&serde_json::json!({ "scale": 1.0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Local tools (no Origin) still work.
    let r = client
        .get(format!("{url}/api/v1/info"))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let r = client.get(format!("{url}/docs")).send().await.unwrap();
    let html = r.text().await.unwrap();
    assert!(!html.contains("unpkg") && html.contains("openapi.json"));
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn token_is_enforced() {
    let (url, shutdown) = start_with(false, Some("s3cret")).await;
    let err = RemoteBackend::new(&url).info().await.unwrap_err();
    assert_eq!(err.kind, mho_service::ErrorKind::Forbidden);
    let ok = RemoteBackend::with_token(&url, Some("s3cret".into()))
        .info()
        .await
        .unwrap();
    assert!(ok.connected);
    let mut rx = RemoteBackend::with_token(&url, Some("s3cret".into()))
        .stream()
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .is_some()
    );
    assert!(RemoteBackend::new(&url).stream().await.is_err());
    assert!(
        RemoteBackend::probe(&url, Duration::from_secs(1)).await,
        "a refusing daemon still owns the scope"
    );
    shutdown.cancel();
}

#[test]
fn network_bind_requires_token() {
    let opts = ServerOptions {
        bind: "0.0.0.0:8954".parse().unwrap(),
        capture_dir: ".".into(),
        api_token: None,
    };
    assert!(opts.validate().is_err());
    let opts = ServerOptions {
        api_token: Some("x".into()),
        ..opts
    };
    assert!(opts.validate().is_ok());
}

// ---- REST coverage --------------------------------------------------------------------------

async fn get(url: &str) -> reqwest::Response {
    reqwest::get(url).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_endpoints_success_and_errors() {
    let (url, shutdown) = start(false).await;
    let v1 = format!("{url}/api/v1");
    let c = reqwest::Client::new();

    // measure via query string, item catalogue, local measurements
    let r: serde_json::Value = get(&format!("{v1}/measure?items=VPP,FREQ&source=CHAN2"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(r.as_array().unwrap().len(), 2);
    assert_eq!(
        get(&format!("{v1}/measure?items=NOPE&source=CHAN2"))
            .await
            .status(),
        400
    );
    let items: serde_json::Value = get(&format!("{v1}/measure/items"))
        .await
        .json()
        .await
        .unwrap();
    assert!(items.as_array().unwrap().len() > 30);
    let local: serde_json::Value = get(&format!("{v1}/measure/local?source=CHAN2"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(local["has_data"], true);
    assert!((local["frequency"].as_f64().unwrap() - 5e3).abs() < 50.0);

    // FFT with window, peak count and full spectrum
    let fft: serde_json::Value = get(&format!(
        "{v1}/fft?source=CHAN4&window=flattop&peaks=2&full=true"
    ))
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(fft["peaks"].as_array().unwrap().len(), 2);
    assert!(fft["dbv"].as_array().unwrap().len() > 100);

    // waveform formats
    let w: serde_json::Value = get(&format!("{v1}/waveform/CHAN1?format=json"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(w["raw"].as_array().unwrap().len(), 1000);
    let csv = get(&format!("{v1}/waveform/2?format=csv"))
        .await
        .text()
        .await
        .unwrap();
    assert!(csv.starts_with("time_s,CHAN2_V"));
    let npy = get(&format!("{v1}/waveform/CHAN1?format=npy")).await;
    assert!(npy.headers().get("x-preamble").is_some());
    assert!(npy.bytes().await.unwrap().starts_with(b"\x93NUMPY"));
    assert_eq!(
        get(&format!("{v1}/waveform/CHAN1?format=xml"))
            .await
            .status(),
        400
    );
    assert_eq!(get(&format!("{v1}/waveform/CHAN9")).await.status(), 400);
    // RAW while running → conflict
    assert_eq!(
        get(&format!("{v1}/waveform/CHAN1?mode=raw")).await.status(),
        409
    );

    // settings
    let ch: serde_json::Value = get(&format!("{v1}/channels/1")).await.json().await.unwrap();
    assert_eq!(ch["channel"], 1);
    let tb: serde_json::Value = c
        .put(format!("{v1}/timebase"))
        .json(&serde_json::json!({"scale": 1e-3}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tb["scale"], 1e-3);
    let r = c
        .put(format!("{v1}/timebase"))
        .json(&serde_json::json!({"scale": -1.0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let tr: serde_json::Value = c
        .put(format!("{v1}/trigger"))
        .json(&serde_json::json!({"edge_level": 0.5, "edge_slope": "Negative"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tr["edge_slope"], "Negative");
    let acq: serde_json::Value = c
        .put(format!("{v1}/acquire"))
        .json(&serde_json::json!({"memory_depth": "100k"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(acq["memory_depth"], 100000.0);
    assert_eq!(
        c.post(format!("{v1}/actions/stop"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        c.post(format!("{v1}/actions/explode"))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

    // live config
    let live: serde_json::Value = get(&format!("{v1}/live")).await.json().await.unwrap();
    assert_eq!(live["enabled"], true);
    let r = c
        .put(format!("{v1}/live"))
        .json(&serde_json::json!({"max_fps": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // options: listing works; the simulator rejects every license → device error
    let opts: serde_json::Value = get(&format!("{v1}/options")).await.json().await.unwrap();
    assert_eq!(opts.as_array().unwrap().len(), 11);
    let r = c
        .post(format!("{v1}/options/install"))
        .json(&serde_json::json!({"key": "MHO900-BWU05T08@abc123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);

    // documentation
    let doc: serde_json::Value = get(&format!("{v1}/openapi.json"))
        .await
        .json()
        .await
        .unwrap();
    let ops: usize = doc["paths"]
        .as_object()
        .unwrap()
        .values()
        .map(|p| p.as_object().unwrap().len())
        .sum();
    assert!(ops >= 25, "{ops} operations");
    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_scope_maps_to_503() {
    let backend = LocalBackend::start(
        Connection::Usbtmc {
            path: Some("/dev/nonexistent-usbtmc".into()),
        },
        false,
        LiveConfig::default(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let opts = ServerOptions {
        bind: addr,
        capture_dir: std::env::temp_dir(),
        api_token: None,
    };
    let shutdown = CancellationToken::new();
    let app = mho_server::router(backend, &opts, shutdown.clone());
    tokio::spawn(mho_server::axum_reexport::serve(listener, app).into_future());
    let r = get(&format!("http://{addr}/api/v1/status")).await;
    assert_eq!(r.status(), 503);
    let info: serde_json::Value = get(&format!("http://{addr}/api/v1/info"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(info["connected"], false);
    shutdown.cancel();
}

// ---- MCP coverage ---------------------------------------------------------------------------

struct Mcp {
    url: String,
    sid: Option<String>,
    client: reqwest::Client,
    id: u64,
}

impl Mcp {
    async fn connect(url: &str) -> Mcp {
        let mut m = Mcp {
            url: format!("{url}/mcp"),
            sid: None,
            client: reqwest::Client::new(),
            id: 0,
        };
        let init = m
            .post(serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}))
            .await;
        m.sid = init
            .headers()
            .get("mcp-session-id")
            .map(|v| v.to_str().unwrap().to_string());
        m.post(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        m
    }

    async fn post(&self, body: serde_json::Value) -> reqwest::Response {
        let mut rb = self
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(s) = &self.sid {
            rb = rb.header("mcp-session-id", s);
        }
        rb.send().await.unwrap()
    }

    /// Calls a tool and returns the `result` object.
    async fn call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let body = self
            .post(
                serde_json::json!({"jsonrpc":"2.0","id":self.id,"method":"tools/call",
                "params":{"name":name,"arguments":args}}),
            )
            .await
            .text()
            .await
            .unwrap();
        let line = body
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .find(|l| l.starts_with('{'))
            .unwrap_or_else(|| body.as_str());
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {body}"));
        v["result"].clone()
    }
}

fn text_of(result: &serde_json::Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_mcp_tool() {
    let (url, shutdown) = start(false).await;
    let mut m = Mcp::connect(&url).await;
    let ok = |r: &serde_json::Value| assert_eq!(r["isError"], false, "{r}");
    let err = |r: &serde_json::Value| assert_eq!(r["isError"], true, "{r}");

    let r = m.call("identify", serde_json::json!({})).await;
    ok(&r);
    assert!(text_of(&r).contains("MHO954"));
    ok(&m.call("get_status", serde_json::json!({})).await);
    ok(&m
        .call(
            "configure_channel",
            serde_json::json!({"channel": 3, "display": true, "scale": 0.5}),
        )
        .await);
    err(&m
        .call(
            "configure_channel",
            serde_json::json!({"channel": 7, "scale": 0.5}),
        )
        .await);
    ok(&m
        .call("configure_timebase", serde_json::json!({"scale": 5e-4}))
        .await);
    ok(&m
        .call(
            "configure_trigger",
            serde_json::json!({"edge_level": 0.2, "sweep": "Normal"}),
        )
        .await);
    ok(&m
        .call(
            "configure_acquisition",
            serde_json::json!({"acq_type": "Peak"}),
        )
        .await);
    ok(&m
        .call("run_control", serde_json::json!({"action": "run"}))
        .await);
    err(&m
        .call("run_control", serde_json::json!({"action": "explode"}))
        .await);
    let r = m
        .call(
            "measure",
            serde_json::json!({"items": ["FREQ"], "source": "CHAN2"}),
        )
        .await;
    ok(&r);
    assert!(text_of(&r).contains("FREQUENCY"));
    err(&m
        .call(
            "measure",
            serde_json::json!({"items": ["RRDELAY"], "source": "CHAN1"}),
        )
        .await);
    ok(&m
        .call("list_measurement_items", serde_json::json!({}))
        .await);
    let r = m
        .call("analyze_waveform", serde_json::json!({"source": "CHAN2"}))
        .await;
    assert!(text_of(&r).contains("\"has_data\": true"));
    let r = m
        .call(
            "fft_peaks",
            serde_json::json!({"source": "CHAN4", "count": 1}),
        )
        .await;
    ok(&r);
    assert!(text_of(&r).contains("peaks"));
    let r = m.call("screenshot", serde_json::json!({})).await;
    assert_eq!(r["content"][0]["type"], "image");
    assert_eq!(r["content"][0]["mimeType"], "image/png");
    ok(&m.call("list_options", serde_json::json!({})).await);
    let r = m.call("get_network", serde_json::json!({})).await;
    assert!(text_of(&r).contains("mac"));
    ok(&m
        .call("configure_network", serde_json::json!({"mode": "Dhcp"}))
        .await);
    err(&m
        .call(
            "configure_network",
            serde_json::json!({"mode": "Static", "ip": "1.2.3"}),
        )
        .await);
    err(&m
        .call("raw_scpi", serde_json::json!({"command": "*IDN?"}))
        .await); // forbidden

    // get_waveform with save_as: file written once, never overwritten, names validated
    let name = format!("mcp-test-{}.csv", std::process::id());
    let r = m
        .call(
            "get_waveform",
            serde_json::json!({"source": "CHAN2", "max_points": 20, "save_as": name}),
        )
        .await;
    ok(&r);
    let t = text_of(&r);
    assert!(t.contains("saved_to") && t.contains(&name), "{t}");
    let saved = std::env::temp_dir()
        .join("mho954-test-captures")
        .join(&name);
    assert!(saved.exists());
    err(&m
        .call(
            "get_waveform",
            serde_json::json!({"source": "CHAN2", "save_as": name}),
        )
        .await);
    err(&m
        .call(
            "get_waveform",
            serde_json::json!({"source": "CHAN2", "save_as": "../evil.csv"}),
        )
        .await);
    std::fs::remove_file(saved).unwrap();
    shutdown.cancel();
}
