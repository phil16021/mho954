//! Device actor and live loop against the simulator, with injected transport faults.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mho_driver::{Action, ChannelUpdate, Connection, Scope, SimTransport};
use mho_scpi::{ReadChunk, Transport};
use mho_service::{Device, ErrorKind, LiveConfig, LocalBackend, ScopeApi};
use tokio::time::{Instant, timeout};

#[derive(Clone, Default)]
struct Ctl {
    /// Simulates an unplugged scope.
    fail: Arc<AtomicBool>,
    /// Queries sent to the scope.
    queries: Arc<AtomicUsize>,
}

struct Flaky {
    inner: SimTransport,
    ctl: Ctl,
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "unplugged")
}

impl Transport for Flaky {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        if self.ctl.fail.load(Ordering::SeqCst) {
            return Err(gone());
        }
        if data.contains(&b'?') {
            self.ctl.queries.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.write_all(data)
    }
    fn read_some(&mut self, buf: &mut [u8]) -> io::Result<ReadChunk> {
        if self.ctl.fail.load(Ordering::SeqCst) {
            return Err(gone());
        }
        self.inner.read_some(buf)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        self.inner.set_timeout(t)
    }
    fn timeout(&self) -> Duration {
        self.inner.timeout()
    }
    fn describe(&self) -> String {
        "flaky-sim".into()
    }
}

fn backend(ctl: &Ctl, live: LiveConfig) -> LocalBackend {
    let c = ctl.clone();
    let device = Device::spawn_with(
        Connection::Sim,
        Box::new(move |_| {
            if c.fail.load(Ordering::SeqCst) {
                return Err(gone().into());
            }
            Scope::from_transport(Box::new(Flaky {
                inner: SimTransport::new(),
                ctl: c.clone(),
            }))
        }),
    );
    LocalBackend::with_device(device, true, live)
}

async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_the_scope_disappears() {
    let ctl = Ctl::default();
    let b = backend(&ctl, LiveConfig::default());
    assert!(b.info().await.unwrap().connected);

    ctl.fail.store(true, Ordering::SeqCst);
    let err = b.status().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unavailable, "{err:?}");
    assert!(!b.device().state().connected);

    ctl.fail.store(false, Ordering::SeqCst);
    // Reconnect happens on the next call after the backoff.
    eventually("reconnect", async || b.status().await.is_ok()).await;
    assert!(b.device().state().connected);
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn panicking_job_does_not_kill_the_device_thread() {
    let ctl = Ctl::default();
    let b = backend(&ctl, LiveConfig::default());
    let r: Result<(), _> = b.device().call(|_| panic!("boom")).await;
    assert!(r.is_err());
    // The scope is reopened on the next call.
    eventually("recovery after panic", async || {
        b.info().await.is_ok_and(|i| i.connected)
    })
    .await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_frames_follow_setting_changes() {
    let ctl = Ctl::default();
    let b = backend(&ctl, LiveConfig::default());
    let mut frames = b.hub().subscribe_frames();
    timeout(Duration::from_secs(3), frames.changed())
        .await
        .unwrap()
        .unwrap();

    b.set_channel(
        2,
        ChannelUpdate {
            scale: Some(0.2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // The settings epoch forces a preamble refresh: CH2 scaling follows within a frame or two.
    eventually("CH2 preamble update", async || {
        let f = frames.borrow_and_update().clone();
        f.is_some_and(|f| {
            f.frame.traces.iter().any(|t| {
                t.source == "CHAN2" && (t.preamble.y_increment - 0.2 / 7500.0).abs() < 1e-9
            })
        })
    })
    .await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_loop_slows_down_when_stopped_and_pauses_without_subscribers() {
    let ctl = Ctl::default();
    let cfg = LiveConfig {
        max_fps: 50.0,
        stopped_fps: 2.0,
        measurements: vec![],
        ..Default::default()
    };
    let b = backend(&ctl, cfg);

    // No subscriber: the scope is not polled.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = ctl.queries.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        ctl.queries.load(Ordering::SeqCst),
        before,
        "polled without subscribers"
    );

    let mut frames = b.hub().subscribe_frames();
    let count = async |frames: &mut tokio::sync::watch::Receiver<_>, d: Duration| {
        let end = Instant::now() + d;
        let mut n = 0;
        while let Ok(Ok(())) = timeout(
            end.saturating_duration_since(Instant::now()),
            frames.changed(),
        )
        .await
        {
            n += 1;
        }
        n
    };
    let running = count(&mut frames, Duration::from_secs(1)).await;
    b.action(Action::Stop).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await; // status poll notices STOP
    let stopped = count(&mut frames, Duration::from_secs(2)).await;
    assert!(running >= 10, "running fps too low: {running}");
    assert!(stopped <= 6, "stopped fps too high: {stopped}");
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_errors_are_published_and_cleared() {
    let ctl = Ctl::default();
    let b = backend(&ctl, LiveConfig::default());
    let _frames = b.hub().subscribe_frames();
    let mut errors = b.hub().subscribe_errors();

    ctl.fail.store(true, Ordering::SeqCst);
    eventually("error published", async || {
        errors.borrow_and_update().is_some()
    })
    .await;
    ctl.fail.store(false, Ordering::SeqCst);
    eventually("error cleared", async || {
        errors.borrow_and_update().is_none()
    })
    .await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_live_config_is_rejected() {
    let b = backend(&Ctl::default(), LiveConfig::default());
    let bad = LiveConfig {
        measure_interval_ms: 10,
        ..Default::default()
    };
    assert_eq!(
        b.set_live_config(bad).await.unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
    let many = LiveConfig {
        measurements: (0..11)
            .map(|_| mho_service::MeasureSpec {
                item: "VPP".into(),
                source: "CHAN1".into(),
            })
            .collect(),
        ..Default::default()
    };
    assert!(b.set_live_config(many).await.is_err());
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_closes_the_scope() {
    let ctl = Ctl::default();
    let b = backend(&ctl, LiveConfig::default());
    assert!(b.info().await.unwrap().connected);
    b.shutdown().await;
    assert!(!b.device().state().connected);
    assert!(b.status().await.is_err(), "device thread must be gone");
}
