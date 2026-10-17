use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use mho_driver::*;
use mho_dsp::format_si;
use mho_server::{McpScope, ServerOptions};
use mho_service::{LocalBackend, MeasureRequest, RemoteBackend, ScopeApi, ScpiRequest};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::Config;
use crate::{Cli, Command};

#[derive(Args)]
pub struct ChannelArgs {
    /// Channel 1..4.
    pub n: u8,
    #[arg(long, conflicts_with = "off")]
    pub on: bool,
    #[arg(long)]
    pub off: bool,
    /// Volts/div.
    #[arg(long)]
    pub scale: Option<f64>,
    /// Offset in volts.
    #[arg(long, allow_hyphen_values = true)]
    pub offset: Option<f64>,
    /// DC | AC | GND.
    #[arg(long)]
    pub coupling: Option<Coupling>,
    /// Bandwidth limit: OFF | 20M | 250M.
    #[arg(long)]
    pub bwl: Option<String>,
    /// Probe ratio, e.g. 10.
    #[arg(long)]
    pub probe: Option<f64>,
    /// 1M | 50.
    #[arg(long)]
    pub impedance: Option<Impedance>,
    #[arg(long)]
    pub invert: Option<bool>,
}

#[derive(Args)]
pub struct TimebaseArgs {
    /// Seconds/div.
    #[arg(long)]
    pub scale: Option<f64>,
    /// Offset in seconds.
    #[arg(long, allow_hyphen_values = true)]
    pub offset: Option<f64>,
    /// MAIN | XY | ROLL.
    #[arg(long)]
    pub mode: Option<String>,
}

#[derive(Args)]
pub struct TriggerArgs {
    /// CHAN1..4, EXT, ...
    #[arg(long)]
    pub source: Option<String>,
    /// rising | falling | either.
    #[arg(long)]
    pub slope: Option<Slope>,
    /// Level in volts.
    #[arg(long, allow_hyphen_values = true)]
    pub level: Option<f64>,
    /// auto | normal | single.
    #[arg(long)]
    pub sweep: Option<TriggerSweep>,
    #[arg(long)]
    pub coupling: Option<String>,
    #[arg(long)]
    pub holdoff: Option<f64>,
}

#[derive(Subcommand)]
pub enum OptionsCmd {
    /// List options and their state.
    List,
    /// Install a purchased option license: MHO900-<OPTION>@<code>, generated on
    /// rigol.com → License Activation from the key you bought and the scope's serial.
    Install { license: String },
}

#[derive(Subcommand)]
pub enum NetCmd {
    /// Show LAN settings and link status.
    Show,
    /// Change LAN settings (applied immediately with :LAN:APPLy).
    Set(NetSetArgs),
}

#[derive(Args)]
pub struct NetSetArgs {
    /// Address assignment via DHCP.
    #[arg(long, group = "mode")]
    pub dhcp: bool,
    /// Link-local Auto-IP (169.254.x.x).
    #[arg(long, group = "mode")]
    pub auto_ip: bool,
    /// Static IP (use with --ip/--mask/--gateway/--dns).
    #[arg(long = "static", group = "mode")]
    pub static_ip: bool,
    #[arg(long)]
    pub ip: Option<String>,
    #[arg(long)]
    pub mask: Option<String>,
    #[arg(long)]
    pub gateway: Option<String>,
    #[arg(long)]
    pub dns: Option<String>,
    /// Enable/disable mDNS (e.g. `--mdns true`).
    #[arg(long)]
    pub mdns: Option<bool>,
    #[arg(long)]
    pub hostname: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Subcommand)]
pub enum FwCmd {
    /// Installed firmware vs. the newest known version.
    Status,
    /// Show the update procedure.
    Update,
}

enum Backend {
    Local(LocalBackend),
    Remote(RemoteBackend),
}

impl Backend {
    fn api(&self) -> &dyn ScopeApi {
        match self {
            Backend::Local(b) => b,
            Backend::Remote(b) => b,
        }
    }
}

fn connection(cfg: &Config) -> anyhow::Result<Connection> {
    Ok(cfg.connection.parse::<Connection>()?)
}

fn daemon_url(cli: &Cli, cfg: &Config) -> String {
    cli.url.clone().unwrap_or_else(|| cfg.url())
}

async fn daemon_running(cli: &Cli, cfg: &Config) -> bool {
    RemoteBackend::probe(&daemon_url(cli, cfg), Duration::from_millis(400)).await
}

/// Uses a running daemon if there is one (it owns the device), otherwise opens the scope.
async fn backend(cli: &Cli, cfg: &Config) -> anyhow::Result<Backend> {
    let explicit = cli.direct || cli.connection.is_some();
    if !explicit && daemon_running(cli, cfg).await {
        info!("using daemon at {}", daemon_url(cli, cfg));
        return Ok(Backend::Remote(RemoteBackend::new(&daemon_url(cli, cfg))));
    }
    let mut live = cfg.live.clone();
    live.enabled = false;
    Ok(Backend::Local(LocalBackend::start(connection(cfg)?, true, live)))
}

async fn ensure_no_daemon(cli: &Cli, cfg: &Config) -> anyhow::Result<()> {
    if !cli.direct && daemon_running(cli, cfg).await {
        bail!(
            "a daemon is running at {} and owns the scope; stop it first (or pass --direct, \
             which risks interleaved responses)",
            daemon_url(cli, cfg)
        );
    }
    Ok(())
}

/// Opens the scope on a blocking thread for direct, low-level commands.
async fn with_scope<R: Send + 'static>(
    cfg: &Config,
    f: impl FnOnce(&mut Scope) -> anyhow::Result<R> + Send + 'static,
) -> anyhow::Result<R> {
    let conn = connection(cfg)?;
    tokio::task::spawn_blocking(move || {
        let mut scope = Scope::open(&conn).with_context(|| format!("opening {conn}"))?;
        f(&mut scope)
    })
    .await?
}

pub async fn run(cli: Cli, cfg: Config) -> anyhow::Result<()> {
    match &cli.command {
        Command::Serve { bind, allow_raw_scpi } => return serve(&cli, cfg.clone(), *bind, *allow_raw_scpi).await,
        Command::Mcp => return mcp_stdio(&cli, &cfg).await,
        Command::Config => {
            print!("{}", toml::to_string_pretty(&cfg)?);
            if let Some(p) = Config::default_path() {
                eprintln!("# config file: {}", p.display());
            }
            return Ok(());
        }
        Command::Bench { count } => {
            ensure_no_daemon(&cli, &cfg).await?;
            let n = *count;
            return with_scope(&cfg, move |s| bench(s, n)).await;
        }
        Command::Probe { output } => {
            ensure_no_daemon(&cli, &cfg).await?;
            let output = output.clone();
            return with_scope(&cfg, move |s| probe(s, output.as_deref())).await;
        }
        Command::Fw { cmd: FwCmd::Update } => {
            print_update_procedure(&cfg);
            return Ok(());
        }
        Command::Measure { list: true, .. } => {
            for it in MEASURE_ITEMS {
                let two = if it.dual_source { " (2 sources)" } else { "" };
                println!("{:<10} {:<6} {}{two}", it.long, it.unit, it.description);
            }
            return Ok(());
        }
        _ => {}
    }

    let backend = backend(&cli, &cfg).await?;
    let result = run_command(cli, &cfg, &backend).await;
    if let Backend::Local(b) = &backend {
        b.shutdown().await;
    }
    result
}

async fn run_command(cli: Cli, cfg: &Config, backend: &Backend) -> anyhow::Result<()> {
    let api = backend.api();
    match cli.command {
        Command::Idn => {
            let info = api.info().await?;
            match &info.idn {
                Some(idn) => println!(
                    "{} {}  serial {}  firmware {}\nvia {}",
                    idn.manufacturer, idn.model, idn.serial, idn.firmware, info.connection
                ),
                None => bail!("scope not connected ({})", info.connection),
            }
        }
        Command::Status { json } => {
            let st = api.status().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                print_status(&st);
            }
        }
        Command::Scpi { command } => match command {
            Some(c) => scpi_once(api, &c).await?,
            None => scpi_repl(api).await?,
        },
        Command::Run => api.action(Action::Run).await?,
        Command::Stop => api.action(Action::Stop).await?,
        Command::Single => api.action(Action::Single).await?,
        Command::Auto => api.action(Action::Autoscale).await?,
        Command::Force => api.action(Action::Force).await?,
        Command::Channel(a) => {
            let display = if a.on { Some(true) } else if a.off { Some(false) } else { None };
            let u = ChannelUpdate {
                display,
                scale: a.scale,
                offset: a.offset,
                coupling: a.coupling,
                bw_limit: a.bwl,
                probe: a.probe,
                impedance: a.impedance,
                invert: a.invert,
                vernier: None,
            };
            let c = if u == ChannelUpdate::default() { api.channel(a.n).await? } else { api.set_channel(a.n, u).await? };
            print_channel(&c);
        }
        Command::Timebase(a) => {
            let u = TimebaseUpdate { scale: a.scale, offset: a.offset, mode: a.mode };
            let t = if u == TimebaseUpdate::default() { api.timebase().await? } else { api.set_timebase(u).await? };
            println!("timebase {}/div  offset {}  mode {}", format_si(t.scale, "s"), format_si(t.offset, "s"), t.mode);
        }
        Command::Trigger(a) => {
            let u = TriggerUpdate {
                mode: None,
                sweep: a.sweep,
                coupling: a.coupling,
                holdoff: a.holdoff,
                edge_source: a.source,
                edge_slope: a.slope,
                edge_level: a.level,
            };
            let t = if u == TriggerUpdate::default() { api.trigger().await? } else { api.set_trigger(u).await? };
            print_trigger(&t);
        }
        Command::Measure { items, source, local, .. } => {
            if local {
                let w = api.waveform(source, WaveMode::Normal).await?;
                print_local(&w.source, &w.analyze());
            } else {
                let items = if items.is_empty() { vec!["VPP".into(), "FREQ".into(), "VRMS".into()] } else { items };
                for m in api.measure(MeasureRequest { items, source, source2: None }).await? {
                    let v = m.value.map(|v| format_si(v, &m.unit)).unwrap_or_else(|| "---".into());
                    println!("{:<10} {:<6} {v}", m.item, m.source);
                }
            }
        }
        Command::Screenshot { output } => {
            let png = api.screenshot().await?;
            std::fs::write(&output, &png)?;
            println!("{} ({} bytes)", output.display(), png.len());
        }
        Command::Capture { source, raw, output } => capture(&backend, source, raw, &output).await?,
        Command::Fft { source, peaks, raw } => {
            let mode = if raw { WaveMode::Raw } else { WaveMode::Normal };
            let w = api.waveform(source, mode).await?;
            let spec = mho_dsp::spectrum(&w.volts(), w.preamble.x_increment, mho_dsp::Window::FlatTop);
            println!("{}: {} points, resolution {}", w.source, w.raw.len(), format_si(spec.df, "Hz"));
            for p in spec.peaks(peaks) {
                println!("  {:>12}  {:>8.2} dBV  {}", format_si(p.frequency, "Hz"), p.dbv, format_si(p.vrms, "Vrms"));
            }
        }
        Command::Options { cmd } => match cmd.unwrap_or(OptionsCmd::List) {
            OptionsCmd::List => {
                for o in api.options().await? {
                    println!("{} {:<10} {}", if o.active { "[x]" } else { "[ ]" }, o.name, o.description);
                }
            }
            OptionsCmd::Install { license } => {
                api.install_option(license).await?;
                println!("key accepted; current options:");
                for o in api.options().await?.iter().filter(|o| o.active) {
                    println!("  [x] {:<10} {}", o.name, o.description);
                }
            }
        },
        Command::Backup { dir } => backup(api, dir.unwrap_or(cfg.backup_dir.clone())).await?,
        Command::Restore { file } => {
            let blob = std::fs::read(&file)?;
            api.setup_load(blob).await?;
            println!("setup restored from {}", file.display());
        }
        Command::Fw { cmd: FwCmd::Status } => fw_status(api, &cfg).await?,
        Command::Net { cmd } => match cmd.unwrap_or(NetCmd::Show) {
            NetCmd::Show => print_network(&api.network().await?),
            NetCmd::Set(a) => {
                let mode = if a.dhcp {
                    Some(IpMode::Dhcp)
                } else if a.auto_ip {
                    Some(IpMode::AutoIp)
                } else if a.static_ip {
                    Some(IpMode::Static)
                } else {
                    None
                };
                let u = NetworkUpdate {
                    mode,
                    ip: a.ip,
                    mask: a.mask,
                    gateway: a.gateway,
                    dns: a.dns,
                    mdns: a.mdns,
                    hostname: a.hostname,
                    description: a.description,
                };
                if u == NetworkUpdate::default() {
                    bail!("nothing to change (see `mho954 net set --help`)");
                }
                print_network(&api.set_network(u).await?);
            }
        },
        Command::Serve { .. }
        | Command::Gui { .. }
        | Command::Mcp
        | Command::Config
        | Command::Bench { .. }
        | Command::Probe { .. }
        | Command::Fw { cmd: FwCmd::Update } => unreachable!("handled above"),
    }
    Ok(())
}

// ---- serve / mcp / gui ------------------------------------------------------------------

/// Resolves on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            match term.as_mut() {
                Some(t) => {
                    t.recv().await;
                }
                None => std::future::pending().await,
            }
        } => {}
    }
}

async fn serve(cli: &Cli, cfg: Config, bind: Option<std::net::SocketAddr>, allow_raw: bool) -> anyhow::Result<()> {
    if daemon_running(cli, &cfg).await && bind.is_none() {
        bail!("a daemon is already running at {}", cfg.url());
    }
    let backend = LocalBackend::start(connection(&cfg)?, allow_raw || cfg.allow_raw_scpi, cfg.live.clone());
    let opts = ServerOptions { bind: bind.unwrap_or(cfg.bind), capture_dir: cfg.capture_dir.clone() };
    if !opts.bind.ip().is_loopback() {
        tracing::warn!("listening on {} without authentication — anyone on the network can control the scope", opts.bind);
    }
    let shutdown = CancellationToken::new();
    let s2 = shutdown.clone();
    tokio::spawn(async move {
        // Ctrl-C and SIGTERM (systemd stop) both shut down gracefully.
        shutdown_signal().await;
        tracing::info!("shutting down");
        s2.cancel();
    });
    let r = mho_server::serve(backend.clone(), opts, shutdown).await;
    backend.shutdown().await;
    r
}

async fn mcp_stdio(cli: &Cli, cfg: &Config) -> anyhow::Result<()> {
    let explicit = cli.direct || cli.connection.is_some();
    let (api, local): (Arc<dyn ScopeApi>, Option<LocalBackend>) =
        if !explicit && daemon_running(cli, cfg).await {
            (Arc::new(RemoteBackend::new(&daemon_url(cli, cfg))), None)
        } else {
            let mut live = cfg.live.clone();
            live.enabled = false;
            // Raw SCPI over MCP only if configured.
            let b = LocalBackend::start(connection(cfg)?, cfg.allow_raw_scpi, live);
            (Arc::new(b.clone()), Some(b))
        };
    let server = McpScope::new(api, cfg.capture_dir.clone());
    let service = server.serve(rmcp::transport::stdio()).await?;
    tokio::select! {
        r = service.waiting() => { r?; }
        _ = shutdown_signal() => {}
    }
    if let Some(b) = local {
        b.shutdown().await;
    }
    Ok(())
}

pub fn gui(cli: &Cli, cfg: Config, sim: bool) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let explicit = cli.direct || cli.connection.is_some() || sim;
    let mut local = None;
    let url = if !explicit && rt.block_on(daemon_running(cli, &cfg)) {
        daemon_url(cli, &cfg)
    } else {
        let conn = if sim { Connection::Sim } else { connection(&cfg)? };
        let backend = rt.block_on(async { LocalBackend::start(conn, cfg.allow_raw_scpi, cfg.live.clone()) });
        // The real scope prefers the configured port so MCP clients find it; the simulator
        // never takes it, so the CLI cannot mistake it for the daemon.
        let listener = if sim {
            rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?
        } else {
            rt.block_on(tokio::net::TcpListener::bind(cfg.bind))
                .or_else(|_| rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")))?
        };
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() {
            tracing::warn!("embedded server listens on {addr} — reachable from the network");
        }
        let opts = ServerOptions { bind: addr, capture_dir: cfg.capture_dir.clone() };
        let shutdown = CancellationToken::new();
        let app = mho_server::router(backend.clone(), &opts, shutdown.clone());
        rt.spawn(async move {
            let _ = axum_serve(listener, app, shutdown).await;
        });
        // A signal must not kill the process while the scope is mid-transfer.
        let b = backend.clone();
        rt.spawn(async move {
            shutdown_signal().await;
            b.shutdown().await;
            std::process::exit(0);
        });
        info!("embedded server on http://{addr} (MCP at /mcp)");
        local = Some(backend);
        format!("http://{addr}")
    };
    let r = mho_gui::run(url, rt.handle().clone(), cfg.capture_dir.clone());
    if let Some(b) = local {
        rt.block_on(b.shutdown());
    }
    r
}

async fn axum_serve(listener: tokio::net::TcpListener, app: mho_server::axum_reexport::Router, shutdown: CancellationToken) -> std::io::Result<()> {
    mho_server::axum_reexport::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await
}

// ---- output helpers -------------------------------------------------------------------------

fn print_channel(c: &ChannelState) {
    println!(
        "CH{} {:<3} {:>10}/div  offset {:>10}  {}  BWL {}  probe {}x  {}{}",
        c.channel,
        if c.display { "ON" } else { "off" },
        format_si(c.scale, "V"),
        format_si(c.offset, "V"),
        c.coupling,
        c.bw_limit,
        c.probe,
        if c.impedance == Impedance::Fifty { "50Ω" } else { "1MΩ" },
        if c.invert { "  inverted" } else { "" }
    );
}

fn print_network(n: &NetworkState) {
    let mode = if n.dhcp {
        "DHCP"
    } else if n.auto_ip {
        "Auto-IP"
    } else if n.manual {
        "static"
    } else {
        "?"
    };
    println!("LAN status   {}", n.status);
    println!("mode         {mode} (dhcp {}, auto-ip {}, static {})", n.dhcp, n.auto_ip, n.manual);
    println!("ip / mask    {} / {}", n.ip, n.mask);
    println!("gateway      {}", n.gateway);
    println!("dns          {}", n.dns);
    println!("dhcp server  {}", n.dhcp_server);
    println!("mac          {}", n.mac);
    println!("hostname     {}  (mDNS {})", n.hostname, if n.mdns { "on" } else { "off" });
    println!("description  {}", n.description);
    println!("visa         {}", n.visa);
    if n.status == "UNLINK" {
        println!("note: :LAN covers the RJ45 port only; a WLAN dongle's address is shown on the scope (Utility → IO).");
    }
}

fn print_trigger(t: &TriggerState) {
    println!(
        "trigger {} {} on {} {} level {}  coupling {}  holdoff {}  status {}",
        t.mode,
        t.sweep,
        t.edge_source,
        t.edge_slope,
        format_si(t.edge_level, "V"),
        t.coupling,
        format_si(t.holdoff, "s"),
        t.status
    );
}

fn print_status(st: &ScopeStatus) {
    println!("{} {} ({})  firmware {}", st.idn.manufacturer, st.idn.model, st.idn.serial, st.idn.firmware);
    println!("status {}", st.trigger_status);
    for c in &st.channels {
        print_channel(c);
    }
    println!(
        "timebase {}/div  offset {}  mode {}",
        format_si(st.timebase.scale, "s"),
        format_si(st.timebase.offset, "s"),
        st.timebase.mode
    );
    print_trigger(&st.trigger);
    println!(
        "acquire {}  {}  depth {} pts  averages {}",
        st.acquire.acq_type,
        format_si(st.acquire.sample_rate, "Sa/s"),
        st.acquire.memory_depth.map(|d| d.to_string()).unwrap_or_else(|| "?".into()),
        st.acquire.averages
    );
}

fn print_local(source: &str, m: &mho_dsp::Measurements) {
    let opt = |v: Option<f64>, u: &str| v.map(|v| format_si(v, u)).unwrap_or_else(|| "---".into());
    println!("{source} ({} samples, computed locally)", m.samples);
    println!("  Vpp {}  Vmax {}  Vmin {}", format_si(m.vpp, "V"), format_si(m.vmax, "V"), format_si(m.vmin, "V"));
    println!("  Vtop {}  Vbase {}  Vamp {}", format_si(m.vtop, "V"), format_si(m.vbase, "V"), format_si(m.vamp, "V"));
    println!("  Vavg {}  Vrms {}  Vrms(AC) {}", format_si(m.vavg, "V"), format_si(m.vrms, "V"), format_si(m.vrms_ac, "V"));
    println!("  freq {}  period {}  duty {}", opt(m.frequency, "Hz"), opt(m.period, "s"), opt(m.duty, "%"));
    println!("  rise {}  fall {}  overshoot {}", opt(m.rise_time, "s"), opt(m.fall_time, "s"), opt(m.overshoot, "%"));
}

// ---- scpi ---------------------------------------------------------------------------------------

async fn scpi_once(api: &dyn ScopeApi, cmd: &str) -> anyhow::Result<()> {
    let r = api.scpi(ScpiRequest { command: cmd.to_string(), timeout_ms: None }).await?;
    if let Some(line) = r.response {
        println!("{line}");
    }
    Ok(())
}

async fn scpi_repl(api: &dyn ScopeApi) -> anyhow::Result<()> {
    let mut rl = rustyline::DefaultEditor::new()?;
    let history = directories::ProjectDirs::from("", "", "mho954").map(|d| d.data_dir().join("scpi_history"));
    if let Some(h) = &history {
        let _ = rl.load_history(h);
    }
    println!("SCPI prompt — queries end in '?', Ctrl-D to quit.");
    loop {
        match rl.readline("scpi> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if let Err(e) = scpi_once(api, line).await {
                    eprintln!("error: {e}");
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(_) => break,
        }
    }
    if let Some(h) = &history {
        let _ = std::fs::create_dir_all(h.parent().unwrap());
        let _ = rl.save_history(h);
    }
    Ok(())
}

// ---- capture ------------------------------------------------------------------------------------

async fn capture(backend: &Backend, source: String, raw: bool, output: &Path) -> anyhow::Result<()> {
    let start = Instant::now();
    let w = match (backend, raw) {
        (Backend::Local(b), true) => {
            let progress = Arc::new(|done: usize, total: usize| {
                eprint!("\r{done}/{total} points ({:.0} %)   ", 100.0 * done as f64 / total.max(1) as f64);
                let _ = std::io::stderr().flush();
            });
            let w = b.raw_waveform(source, progress).await?;
            eprintln!();
            w
        }
        (_, raw) => backend.api().waveform(source, if raw { WaveMode::Raw } else { WaveMode::Normal }).await?,
    };
    let secs = start.elapsed().as_secs_f64();
    let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "csv" => {
            let volts = w.volts();
            let file = std::io::BufWriter::new(std::fs::File::create(output)?);
            mho_dsp::export::write_csv(
                file,
                w.preamble.time(0),
                w.preamble.x_increment,
                &[mho_dsp::export::CsvColumn { name: &w.source, volts: &volts }],
            )?;
        }
        "npy" => {
            mho_dsp::export::write_npy(output, &w.raw)?;
            let meta = serde_json::json!({ "source": w.source, "preamble": w.preamble });
            std::fs::write(output.with_extension("json"), serde_json::to_vec_pretty(&meta)?)?;
        }
        "json" => std::fs::write(output, serde_json::to_vec(&w)?)?,
        other => bail!("unsupported output extension {other:?} (use .csv, .npy or .json)"),
    }
    println!(
        "{}: {} points of {} in {:.2} s → {}",
        if raw { "RAW" } else { "screen" },
        w.raw.len(),
        w.source,
        secs,
        output.display()
    );
    Ok(())
}

// ---- backup / firmware --------------------------------------------------------------------------

async fn backup(api: &dyn ScopeApi, base: std::path::PathBuf) -> anyhow::Result<()> {
    let dir = base.join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
    std::fs::create_dir_all(&dir)?;
    let info = api.info().await?;
    std::fs::write(dir.join("info.json"), serde_json::to_vec_pretty(&info)?)?;
    let status = api.status().await?;
    std::fs::write(dir.join("status.json"), serde_json::to_vec_pretty(&status)?)?;
    let options = api.options().await?;
    std::fs::write(dir.join("options.json"), serde_json::to_vec_pretty(&options)?)?;
    let setup = api.setup_save().await?;
    std::fs::write(dir.join("setup.bin"), &setup)?;
    match api.screenshot().await {
        Ok(png) => std::fs::write(dir.join("screenshot.png"), png)?,
        Err(e) => eprintln!("screenshot skipped: {e}"),
    }
    println!("backup written to {}", dir.display());
    println!("  setup.bin ({} bytes) can be restored with `mho954 restore {}`", setup.len(), dir.join("setup.bin").display());
    println!(
        "  note: calibration data and license files live on the scope's internal storage and are \
         not reachable via SCPI"
    );
    Ok(())
}

async fn fw_status(api: &dyn ScopeApi, cfg: &Config) -> anyhow::Result<()> {
    let info = api.info().await?;
    let idn = info.idn.context("scope not connected")?;
    println!("model     {}", idn.model);
    println!("serial    {}", idn.serial);
    println!("installed {}", idn.firmware);
    println!("latest    {} (from config `latest_firmware`; check rigol.com)", cfg.latest_firmware);
    let short = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (inst, latest) = (short(&idn.firmware), short(&cfg.latest_firmware));
    if inst.len() >= latest.len() && inst >= latest {
        println!("→ up to date");
    } else if inst.len() < latest.len() && latest.starts_with(&inst) {
        println!("→ *IDN? reports a shortened version; compare in Utility → System → About on the scope");
    } else {
        println!("→ update available, see `mho954 fw update`");
    }
    Ok(())
}

fn print_update_procedure(cfg: &Config) {
    println!(
        "Firmware update (Rigol MHO900 series)\n\
         \n\
         The update runs on the scope from a USB stick; it cannot be pushed over SCPI.\n\
         \n\
         1. Back up first:          mho954 backup\n\
         2. Download the firmware for the MHO900 series from rigol.com (Support → Software/Firmware).\n\
            Newest known version in your config: {}\n\
         3. Copy the .GEL file to the root of an empty FAT32-formatted USB stick.\n\
         4. Plug the stick into the scope's front USB host port.\n\
         5. On the scope: Utility → System → Upgrade (or the update dialog that pops up), confirm.\n\
         6. Do not power off during the update; the scope restarts by itself.\n\
         7. Verify:                 mho954 fw status\n\
         \n\
         Release notes can change scope behaviour (e.g. bandwidth limit filters in .25/.26);\n\
         re-check measurements that matter to you after updating.",
        cfg.latest_firmware
    );
}

// ---- bench / probe ------------------------------------------------------------------------------

fn stats(samples: &[f64]) -> String {
    if samples.is_empty() {
        return "-".into();
    }
    let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = samples.iter().copied().fold(0.0, f64::max);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    format!("mean {:.1} ms  min {:.1}  max {:.1}", mean * 1e3, min * 1e3, max * 1e3)
}

fn time<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64())
}

fn bench(s: &mut Scope, n: usize) -> anyhow::Result<()> {
    println!("{} via {}", s.idn().model, s.describe());
    let mut t = Vec::new();
    for _ in 0..n {
        let (r, dt) = time(|| s.scpi().query("*IDN?"));
        r?;
        t.push(dt);
    }
    println!("*IDN?                {}", stats(&t));

    let pre = s.preamble("CHAN1", WaveMode::Normal)?;
    let mut t = Vec::new();
    for _ in 0..n {
        let (r, dt) = time(|| s.screen_waveform("CHAN1", Some(pre)));
        r?;
        t.push(dt);
    }
    println!("screen waveform CH1  {}  ({} pts)", stats(&t), pre.points);

    let (r, dt) = time(|| s.measure("VPP", "CHAN1", None));
    r?;
    println!(":MEAS:ITEM? VPP      {:.1} ms", dt * 1e3);

    let (png, dt) = time(|| s.screenshot());
    let png = png?;
    println!("screenshot           {:.0} ms, {} bytes, {:.2} MB/s", dt * 1e3, png.len(), png.len() as f64 / dt / 1e6);

    if s.trigger_status()? == "STOP" {
        let (w, dt) = time(|| s.raw_waveform("CHAN1", &mut |_, _| {}));
        let w = w?;
        println!(
            "RAW CH1              {:.2} s, {} pts, {:.2} MB/s",
            dt,
            w.raw.len(),
            w.raw.len() as f64 * 2.0 / dt / 1e6
        );
    } else {
        println!("RAW CH1              skipped (scope running; `mho954 stop` to include)");
    }
    Ok(())
}

fn probe(s: &mut Scope, output: Option<&Path>) -> anyhow::Result<()> {
    let queries = [
        "*IDN?",
        "*OPC?",
        ":SYST:VERS?",
        ":SYST:ERR?",
        ":TRIG:STAT?",
        ":ACQ:SRAT?",
        ":ACQ:MDEP?",
        ":TIM:SCAL?",
        ":CHAN1:SCAL?",
        ":CHAN1:BWL?",
        ":WAV:FORM?",
        ":WAV:MODE?",
        ":WAV:PRE?",
        ":MEAS:ITEM? VPP,CHAN1",
        ":SYST:OPT:VAL? BWU05T08",
        ":LAN:IPAD?",
    ];
    let mut report = serde_json::Map::new();
    report.insert("idn".into(), serde_json::to_value(s.idn())?);
    report.insert("transport".into(), s.describe().into());
    let mut results = Vec::new();
    for q in queries {
        let (r, dt) = time(|| s.raw(q, Duration::from_millis(1500)));
        let (ok, resp) = match r {
            Ok(v) => (true, v.unwrap_or_default()),
            Err(e) => (false, e.to_string()),
        };
        println!("{:<28} {:>7.1} ms  {}{}", q, dt * 1e3, if ok { "" } else { "ERROR " }, resp);
        results.push(serde_json::json!({ "query": q, "ok": ok, "ms": dt * 1e3, "response": resp }));
    }
    report.insert("queries".into(), results.into());

    // Waveform transfer and data alignment check.
    let (w, dt) = time(|| s.screen_waveform("CHAN1", None));
    match w {
        Ok(w) => {
            let (min, max) = w.raw.iter().fold((u16::MAX, 0u16), |(a, b), &x| (a.min(x), b.max(x)));
            let low_bits_used = w.raw.iter().any(|&x| x & 0x000f != 0);
            println!(
                "screen waveform CH1          {:>7.1} ms  {} pts, codes {min}..{max}, low nibble used: {low_bits_used}",
                dt * 1e3,
                w.raw.len()
            );
            report.insert(
                "waveform".into(),
                serde_json::json!({ "ms": dt * 1e3, "points": w.raw.len(), "min": min, "max": max,
                    "low_nibble_used": low_bits_used, "preamble": w.preamble }),
            );
        }
        Err(e) => {
            println!("screen waveform CH1          ERROR {e}");
            report.insert("waveform".into(), serde_json::json!({ "error": e.to_string() }));
        }
    }
    let (png, dt) = time(|| s.screenshot());
    match png {
        Ok(p) => {
            println!("screenshot                   {:>7.1} ms  {} bytes", dt * 1e3, p.len());
            report.insert("screenshot".into(), serde_json::json!({ "ms": dt * 1e3, "bytes": p.len() }));
        }
        Err(e) => {
            println!("screenshot                   ERROR {e}");
            report.insert("screenshot".into(), serde_json::json!({ "error": e.to_string() }));
        }
    }
    if let Some(path) = output {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        println!("report written to {}", path.display());
    }
    Ok(())
}
