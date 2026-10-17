//! HTTP client for a running `mho954 serve` daemon.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use mho_driver::*;
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;

use crate::api::*;
use crate::live::{LiveConfig, LiveEvent, WsText, decode_frame};

#[derive(Clone)]
pub struct RemoteBackend {
    base: String,
    client: reqwest::Client,
    token: Option<String>,
}

impl RemoteBackend {
    /// `base` like `http://127.0.0.1:8954`.
    pub fn new(base: &str) -> Self {
        Self::with_token(base, None)
    }

    /// Client that sends `Authorization: Bearer <token>` (needed for daemons on the network).
    pub fn with_token(base: &str, token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        RemoteBackend {
            base: base.trim_end_matches('/').to_string(),
            client,
            token: token.filter(|t| !t.is_empty()),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// Returns `true` if an mho954 server answers at `base` within `timeout`, including one
    /// that refuses the request (401/403): it still owns the device.
    pub async fn probe(base: &str, timeout: Duration) -> bool {
        let url = format!("{}/api/v1/info", base.trim_end_matches('/'));
        let Ok(client) = reqwest::Client::builder().timeout(timeout).build() else {
            return false;
        };
        match client.get(url).send().await {
            Ok(r) => r.status().is_success() || matches!(r.status().as_u16(), 401 | 403),
            Err(_) => false,
        }
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let rb = self
            .client
            .request(method, format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    async fn check(resp: Result<Response, reqwest::Error>) -> ApiResult<Response> {
        let resp = resp.map_err(|e| ApiError::unavailable(format!("daemon unreachable: {e}")))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(serde_json::from_str::<ApiError>(&body).unwrap_or_else(|_| {
            ApiError::new(ErrorKind::Internal, format!("HTTP {status}: {body}"))
        }))
    }

    async fn json<T: DeserializeOwned>(&self, rb: RequestBuilder) -> ApiResult<T> {
        let resp = Self::check(rb.send().await).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ApiError::new(ErrorKind::Internal, format!("bad response: {e}")))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        self.json(self.req(Method::GET, path)).await
    }

    async fn send<B: Serialize + Sync, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> ApiResult<T> {
        self.json(self.req(method, path).json(body)).await
    }

    async fn bytes(&self, rb: RequestBuilder) -> ApiResult<Vec<u8>> {
        let resp = Self::check(rb.send().await).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ApiError::new(ErrorKind::Internal, format!("bad response: {e}")))
    }

    async fn empty(&self, rb: RequestBuilder) -> ApiResult<()> {
        Self::check(rb.send().await).await.map(|_| ())
    }

    /// Subscribes to the live WebSocket. The channel closes when the connection drops.
    pub async fn stream(&self) -> ApiResult<mpsc::Receiver<LiveEvent>> {
        let url = format!("{}/api/v1/stream", self.base.replacen("http", "ws", 1));
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| ApiError::invalid(format!("websocket {url}: {e}")))?;
        if let Some(t) = &self.token {
            let value = format!("Bearer {t}")
                .parse()
                .map_err(|_| ApiError::invalid("invalid API token"))?;
            request.headers_mut().insert("authorization", value);
        }
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| ApiError::unavailable(format!("websocket {url}: {e}")))?;
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let (_sink, mut stream) = ws.split();
            while let Some(msg) = stream.next().await {
                let event = match msg {
                    Ok(Message::Binary(b)) => match decode_frame(&b) {
                        Ok(f) => LiveEvent::Frame(Arc::new(f)),
                        Err(e) => LiveEvent::Error(format!("bad frame: {e}")),
                    },
                    Ok(Message::Text(t)) => match serde_json::from_str::<WsText>(&t) {
                        Ok(WsText::Measurements(m)) => LiveEvent::Measurements(Arc::new(m)),
                        Ok(WsText::Error { message }) => LiveEvent::Error(message),
                        Err(e) => {
                            debug!("ignoring ws text: {e}");
                            continue;
                        }
                    },
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

#[async_trait]
impl ScopeApi for RemoteBackend {
    async fn info(&self) -> ApiResult<ServiceInfo> {
        self.get("/info").await
    }

    async fn status(&self) -> ApiResult<ScopeStatus> {
        self.get("/status").await
    }

    async fn channel(&self, n: u8) -> ApiResult<ChannelState> {
        self.get(&format!("/channels/{n}")).await
    }

    async fn set_channel(&self, n: u8, update: ChannelUpdate) -> ApiResult<ChannelState> {
        self.send(Method::PUT, &format!("/channels/{n}"), &update)
            .await
    }

    async fn timebase(&self) -> ApiResult<TimebaseState> {
        self.get("/timebase").await
    }

    async fn set_timebase(&self, update: TimebaseUpdate) -> ApiResult<TimebaseState> {
        self.send(Method::PUT, "/timebase", &update).await
    }

    async fn trigger(&self) -> ApiResult<TriggerState> {
        self.get("/trigger").await
    }

    async fn set_trigger(&self, update: TriggerUpdate) -> ApiResult<TriggerState> {
        self.send(Method::PUT, "/trigger", &update).await
    }

    async fn acquire(&self) -> ApiResult<AcquireState> {
        self.get("/acquire").await
    }

    async fn set_acquire(&self, update: AcquireUpdate) -> ApiResult<AcquireState> {
        self.send(Method::PUT, "/acquire", &update).await
    }

    async fn action(&self, action: Action) -> ApiResult<()> {
        let name = format!("{action:?}").to_ascii_lowercase();
        self.empty(self.req(Method::POST, &format!("/actions/{name}")))
            .await
    }

    async fn measure(&self, request: MeasureRequest) -> ApiResult<Vec<Measurement>> {
        self.send(Method::POST, "/measure", &request).await
    }

    async fn waveform(&self, source: String, mode: WaveMode) -> ApiResult<Waveform> {
        let mode = if mode == WaveMode::Raw { "raw" } else { "norm" };
        let rb = self
            .req(Method::GET, &format!("/waveform/{source}"))
            .query(&[("mode", mode), ("format", "bin")]);
        let bytes = self.bytes(rb).await?;
        let frame = decode_frame(&bytes).map_err(|e| ApiError::new(ErrorKind::Internal, e))?;
        frame
            .traces
            .into_iter()
            .next()
            .ok_or_else(|| ApiError::new(ErrorKind::Internal, "empty waveform"))
    }

    async fn screenshot(&self) -> ApiResult<Vec<u8>> {
        self.bytes(self.req(Method::GET, "/screenshot.png")).await
    }

    async fn options(&self) -> ApiResult<Vec<OptionInfo>> {
        self.get("/options").await
    }

    async fn install_option(&self, key: String) -> ApiResult<()> {
        self.empty(
            self.req(Method::POST, "/options/install")
                .json(&serde_json::json!({ "key": key })),
        )
        .await
    }

    async fn setup_save(&self) -> ApiResult<Vec<u8>> {
        self.bytes(self.req(Method::GET, "/setup")).await
    }

    async fn setup_load(&self, blob: Vec<u8>) -> ApiResult<()> {
        self.empty(self.req(Method::PUT, "/setup").body(blob)).await
    }

    async fn scpi(&self, request: ScpiRequest) -> ApiResult<ScpiResponse> {
        self.send(Method::POST, "/scpi", &request).await
    }

    async fn measure_stats(&self, r: StatsRequest) -> ApiResult<MeasureStats> {
        let mut q = vec![("item", r.item), ("source", r.source)];
        if let Some(s2) = r.source2 {
            q.push(("source2", s2));
        }
        self.json(self.req(Method::GET, "/measure/stats").query(&q))
            .await
    }

    async fn measure_action(&self, action: MeasureAction) -> ApiResult<()> {
        let name = action.scpi().to_ascii_lowercase();
        self.empty(self.req(Method::POST, &format!("/measure/actions/{name}")))
            .await
    }

    async fn counter_value(&self) -> ApiResult<Option<f64>> {
        self.get("/counter").await
    }

    async fn dvm_value(&self) -> ApiResult<Option<f64>> {
        self.get("/dvm").await
    }

    async fn modules(&self) -> ApiResult<Modules> {
        self.get("/modules").await
    }

    async fn groups(&self) -> ApiResult<Vec<mho_driver::params::GroupInfo>> {
        self.get("/groups").await
    }

    async fn group_get(
        &self,
        id: String,
        instance: Option<u8>,
    ) -> ApiResult<mho_driver::params::ParamMap> {
        let mut rb = self.req(Method::GET, &format!("/groups/{id}"));
        if let Some(n) = instance {
            rb = rb.query(&[("instance", n)]);
        }
        self.json(rb).await
    }

    async fn group_set(
        &self,
        id: String,
        instance: Option<u8>,
        values: mho_driver::params::ParamMap,
        activate: bool,
    ) -> ApiResult<Option<mho_driver::params::ParamMap>> {
        let mut rb = self
            .req(Method::PUT, &format!("/groups/{id}"))
            .query(&[("activate", activate)]);
        if let Some(n) = instance {
            rb = rb.query(&[("instance", n)]);
        }
        self.json(rb.json(&values)).await
    }

    async fn network(&self) -> ApiResult<NetworkState> {
        self.get("/network").await
    }

    async fn set_network(&self, update: NetworkUpdate) -> ApiResult<NetworkState> {
        self.send(Method::PUT, "/network", &update).await
    }

    async fn live_config(&self) -> ApiResult<LiveConfig> {
        self.get("/live").await
    }

    async fn set_live_config(&self, config: LiveConfig) -> ApiResult<LiveConfig> {
        self.send(Method::PUT, "/live", &config).await
    }
}
