//! End-to-end: server on an ephemeral port with the simulator, exercised through the
//! HTTP client backend, the live WebSocket and the MCP endpoint.

use std::time::Duration;

use mho_driver::{Action, ChannelUpdate, Connection, WaveMode};
use mho_server::ServerOptions;
use mho_service::{LiveConfig, LiveEvent, LocalBackend, MeasureRequest, RemoteBackend, ScopeApi, ScpiRequest};
use tokio_util::sync::CancellationToken;

async fn start(allow_raw: bool) -> (String, CancellationToken) {
    let backend = LocalBackend::start(Connection::Sim, allow_raw, LiveConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let opts = ServerOptions { bind: addr, capture_dir: std::env::temp_dir().join("mho954-test-captures") };
    let shutdown = CancellationToken::new();
    let app = mho_server::router(backend, &opts, shutdown.clone());
    let s = shutdown.clone();
    tokio::spawn(async move {
        mho_server::axum_reexport::serve(listener, app).with_graceful_shutdown(s.cancelled_owned()).await.unwrap();
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

    let ch = api.set_channel(3, ChannelUpdate { display: Some(true), scale: Some(0.5), ..Default::default() }).await.unwrap();
    assert!(ch.display);
    assert_eq!(ch.scale, 0.5);

    let err = api.set_channel(9, ChannelUpdate::default()).await.unwrap_err();
    assert_eq!(err.kind, mho_service::ErrorKind::InvalidArgument);

    let m = api
        .measure(MeasureRequest { items: vec!["FREQ".into()], source: "1".into(), source2: None })
        .await
        .unwrap();
    assert!((m[0].value.unwrap() - 1000.0).abs() < 5.0, "{m:?}");

    let w = api.waveform("CHAN2".into(), WaveMode::Normal).await.unwrap();
    assert_eq!(w.raw.len(), 1000);

    api.action(Action::Stop).await.unwrap();
    let raw = api.waveform("CHAN1".into(), WaveMode::Raw).await.unwrap();
    assert_eq!(raw.raw.len(), 10_000);
    api.action(Action::Run).await.unwrap();

    let png = api.screenshot().await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));

    let r = api.scpi(ScpiRequest { command: ":TIM:SCAL?".into(), timeout_ms: None }).await.unwrap();
    assert!(r.response.unwrap().contains("E-4"));

    let setup = api.setup_save().await.unwrap();
    api.setup_load(setup).await.unwrap();

    shutdown.cancel();
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_scpi_forbidden_by_default() {
    let (url, shutdown) = start(false).await;
    let api = RemoteBackend::new(&url);
    let err = api.scpi(ScpiRequest { command: "*IDN?".into(), timeout_ms: None }).await.unwrap_err();
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
    assert!(frames >= 3 && meas >= 1, "frames {frames}, measurements {meas}");
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
    let sid = init.headers().get("mcp-session-id").map(|v| v.to_str().unwrap().to_string());
    let body = init.text().await.unwrap();
    assert!(body.contains("\"name\":\"mho954\""), "{body}");

    post(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}), sid.clone()).await.unwrap();
    let list = post(serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), sid.clone())
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for tool in ["get_status", "measure", "get_waveform", "screenshot", "fft_peaks"] {
        assert!(list.contains(&format!("\"name\":\"{tool}\"")), "missing {tool}");
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
    assert!(call.contains("frequency") && call.contains("\"isError\":false"), "{call}");
    shutdown.cancel();
}
