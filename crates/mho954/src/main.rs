//! `mho954` — command-line entry point.

mod commands;
mod config;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser)]
#[command(
    name = "mho954",
    version,
    about = "Control the Rigol MHO954 oscilloscope: live view, REST API, MCP server, CLI"
)]
pub struct Cli {
    /// Config file (default ~/.config/mho954/config.toml).
    #[arg(long, global = true, env = "MHO954_CONFIG")]
    config: Option<PathBuf>,
    /// Scope connection: usb[:SERIAL] | usbtmc | /dev/usbtmcN | tcp:HOST[:PORT] | sim. Implies --direct.
    #[arg(short, long, global = true, env = "MHO954_CONNECTION")]
    connection: Option<String>,
    /// Daemon URL (default derived from `bind` in the config).
    #[arg(long, global = true, env = "MHO954_URL")]
    url: Option<String>,
    /// Talk to the scope directly even if a daemon is running.
    #[arg(long, global = true)]
    direct: bool,
    /// More log output (-v info, -vv debug, -vvv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon: REST API, live WebSocket and MCP server.
    Serve {
        /// Listen address, e.g. 127.0.0.1:8954.
        #[arg(long)]
        bind: Option<std::net::SocketAddr>,
        /// Allow raw SCPI over REST/MCP.
        #[arg(long)]
        allow_raw_scpi: bool,
    },
    /// Open the live view (starts an embedded server unless a daemon is running).
    Gui {
        /// Use the built-in simulator instead of the scope.
        #[arg(long)]
        sim: bool,
    },
    /// MCP server on stdio (for MCP clients that spawn a process).
    Mcp,
    /// Show identification and connection.
    Idn,
    /// Show what the scope can do: bandwidth with upgrades, memory limits, generator, options.
    Caps {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the full scope state.
    Status {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Send SCPI; without argument starts an interactive prompt.
    Scpi { command: Option<String> },
    /// Start acquisition.
    Run,
    /// Stop acquisition.
    Stop,
    /// Arm a single acquisition.
    Single,
    /// Autoscale.
    Auto,
    /// Force a trigger.
    Force,
    /// Clear the waveforms on the scope screen.
    Clear,
    /// Show the acquisition, or change it: `acquire --depth 500M --type average --averages 16`.
    Acquire {
        /// AUTO, 1k ... 500M (limits per active channel count, see `caps`).
        #[arg(long)]
        depth: Option<String>,
        /// normal | peak | average | hres.
        #[arg(long = "type")]
        acq_type: Option<mho_driver::AcquireType>,
        /// Averages (power of two) for type average.
        #[arg(long)]
        averages: Option<u32>,
    },
    /// Configure an analog channel.
    Channel(commands::ChannelArgs),
    /// Configure the timebase.
    Timebase(commands::TimebaseArgs),
    /// Configure the edge trigger.
    Trigger(commands::TriggerArgs),
    /// Measurements, e.g. `measure VPP FREQ -s 1`.
    Measure {
        /// Items (see `measure --list`).
        items: Vec<String>,
        #[arg(short, long, default_value = "CHAN1")]
        source: String,
        /// Second source for delay/phase items (RRDelay, RRPHase, ...).
        #[arg(long)]
        source2: Option<String>,
        /// Compute locally from the screen waveform instead of asking the scope.
        #[arg(long)]
        local: bool,
        /// List available items.
        #[arg(long)]
        list: bool,
    },
    /// Save a screenshot.
    Screenshot {
        /// PNG file to write.
        #[arg(short, long, default_value = "screenshot.png")]
        output: PathBuf,
    },
    /// Capture a waveform to .csv, .npy (+ .json preamble) or .json.
    Capture {
        #[arg(short, long, default_value = "CHAN1")]
        source: String,
        /// Full acquisition memory (scope must be stopped; use `single` first).
        #[arg(long)]
        raw: bool,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Spectrum peaks of a channel (computed locally).
    Fft {
        #[arg(short, long, default_value = "CHAN1")]
        source: String,
        /// Number of peaks.
        #[arg(short = 'n', long, default_value_t = 5)]
        peaks: usize,
        /// Use the full acquisition memory (scope stopped) instead of the screen data.
        #[arg(long)]
        raw: bool,
        /// rectangular | hann | blackman | flattop (flattop: accurate amplitudes).
        #[arg(long, default_value = "flattop")]
        window: String,
    },
    /// Software options; without subcommand: list them.
    Options {
        #[command(subcommand)]
        cmd: Option<commands::OptionsCmd>,
    },
    /// Back up identification, options, setup and a screenshot.
    Backup {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Restore a setup saved by `backup` (setup.bin).
    Restore { file: PathBuf },
    /// LAN (RJ45) settings: without subcommand show them, or change with `net set ...` (WLAN has
    /// no SCPI interface).
    Net {
        #[command(subcommand)]
        cmd: Option<commands::NetCmd>,
    },
    /// Statistics of a measurement, e.g. `stats VPP -s 1`.
    Stats {
        item: String,
        #[arg(short, long, default_value = "CHAN1")]
        source: String,
        /// Second source for delay/phase items.
        #[arg(long)]
        source2: Option<String>,
        /// Reset the statistics first.
        #[arg(long)]
        reset: bool,
        /// Remove all measurements from the scope screen first.
        #[arg(long)]
        clear_all: bool,
    },
    /// Hardware frequency counter reading (configure with `set counter enable=true ...`).
    Counter {
        /// Clear the totalizer.
        #[arg(long)]
        clear_total: bool,
    },
    /// Digital voltmeter reading (configure with `set dvm enable=true ...`).
    Dvm,
    /// Read a value set: cursor.manual, cursor.track, mask, search, histogram, save, afg.arb
    /// (`--enable` switches the mode on if it is off).
    Read {
        id: Option<String>,
        /// List the value sets.
        #[arg(long)]
        list: bool,
        /// Switch the mode on first if it is off (histogram, mask test, search, cursor).
        #[arg(long)]
        enable: bool,
    },
    /// Run a one-shot command (save/load on the scope, resets, recording, references, generator
    /// align/arb load); `--list` shows them with their preconditions.
    Do {
        id: Option<String>,
        /// File on the scope for save/load (C:/ internal memory, D:/ USB stick in the scope), e.g.
        /// C:/bench1.stp, or the number for ref.save/ref.reset.
        path: Option<String>,
        #[arg(long)]
        list: bool,
        /// Confirm destructive commands (reset).
        #[arg(long)]
        yes: bool,
    },
    /// Digital channels D0..D15 through decoder `bus` (logic probe PLA2216 needed; the decoder
    /// is restored afterwards).
    Logic {
        #[arg(long, default_value_t = 4)]
        bus: u8,
        /// Write a VCD file (PulseView/sigrok, GTKWave) instead of printing.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Built-in function generator (AFG option): show channel 1 (GI) or 2 (GII), or change it,
    /// e.g. `afg 1 --func sin --freq 1k --ampl 1 --load 50 --on`.
    Afg {
        /// 1 = GI, 2 = GII.
        channel: u8,
        /// Switch the output on (after the other settings).
        #[arg(long)]
        on: bool,
        /// Switch the output off (before the other settings).
        #[arg(long, conflicts_with = "on")]
        off: bool,
        /// SIN, SQU, RAMP, NOIS, DC, ARB, EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC.
        #[arg(long)]
        func: Option<String>,
        /// Frequency in Hz (SI prefixes: 1k, 2.5M).
        #[arg(long)]
        freq: Option<String>,
        /// Amplitude in Vpp.
        #[arg(long)]
        ampl: Option<String>,
        /// DC offset in V.
        #[arg(long, allow_hyphen_values = true)]
        offset: Option<String>,
        /// Start phase in degrees.
        #[arg(long)]
        phase: Option<String>,
        /// Square duty cycle in %.
        #[arg(long)]
        duty: Option<String>,
        /// Ramp symmetry in %.
        #[arg(long)]
        symmetry: Option<String>,
        /// Expected load: highz or 50.
        #[arg(long)]
        load: Option<String>,
        /// Modulation: am, fm, pm or off.
        #[arg(long = "mod")]
        modulation: Option<String>,
        /// AM depth in %.
        #[arg(long)]
        depth: Option<String>,
        /// FM deviation (Hz) or PM deviation (°).
        #[arg(long)]
        deviation: Option<String>,
        /// Modulating frequency in Hz.
        #[arg(long)]
        mod_freq: Option<String>,
        /// Modulating waveform: SIN, SQU, TRI, UPR, DNR, NOIS.
        #[arg(long)]
        mod_wave: Option<String>,
    },
    /// Bode plot with the built-in generator (AFG option): software sweep with data and CSV, or
    /// with --native the scope's own plot as screenshot.
    Bode {
        /// Start frequency in Hz (SI prefixes: 100, 1k; native 10 Hz..3 MHz and <= stop/10).
        #[arg(long, default_value = "100")]
        start: String,
        /// Stop frequency in Hz (up to the generator's sine limit; native up to 30 MHz).
        #[arg(long, default_value = "1M")]
        stop: String,
        /// Log-spaced points (2..200); with --native points per decade (10..100).
        #[arg(long, default_value_t = 31)]
        points: usize,
        /// Channel at the DUT input.
        #[arg(long, default_value_t = 1)]
        dut_in: u8,
        /// Channel at the DUT output.
        #[arg(long, default_value_t = 2)]
        dut_out: u8,
        /// Generator amplitude in Vpp.
        #[arg(long, default_value = "1")]
        amplitude: String,
        /// Generator 1 (GI) or 2 (GII).
        #[arg(long, default_value_t = 1)]
        generator: u8,
        /// Wait after each frequency step in ms.
        #[arg(long, default_value_t = 100)]
        settle: u64,
        /// Load the generator expects: highz or 50.
        #[arg(long)]
        load: Option<String>,
        /// Acquisitions averaged per frequency (1..16).
        #[arg(long, default_value_t = 1)]
        averages: u32,
        /// Drive the other generator in step (same sine, phases aligned) to compare two
        /// signal paths, e.g. GI->CH1 against GII->CH2 without a T piece.
        #[arg(long)]
        follow: Option<u8>,
        /// Run the scope's own Bode plot instead and save its screenshot (-o file.png).
        /// `--points` is then per decade (10..100).
        #[arg(long)]
        native: bool,
        /// With --native: leave the plot on the scope's screen (Bode mode stays on, the two
        /// channels keep its AC coupling, 1 mV/div and 20 MHz limit).
        #[arg(long, requires = "native")]
        keep: bool,
        /// With --native: linear instead of logarithmic frequency steps.
        #[arg(long, requires = "native")]
        linear: bool,
        /// With --native: give up after this many seconds.
        #[arg(long, requires = "native", default_value_t = 300)]
        timeout: u64,
        /// Write CSV (frequency, gain, phase, unwrapped phase; summary as # lines).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Peak table of the scope's FFT on math channel 1..4 (`set math.fft --activate peak_search=true`).
    Peaks { math: u8 },
    /// Decoded events of protocol decoder 1..4 (set it up with `set bus.rs232 --activate ...`).
    Decode {
        bus: u8,
        /// Print CSV instead of a table.
        #[arg(long)]
        csv: bool,
        /// Write CSV to this file.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// List settings groups (trigger types, decoders, …), optionally by prefix.
    Groups {
        prefix: Option<String>,
        /// Show the parameters of each group.
        #[arg(long)]
        params: bool,
    },
    /// Read a settings group, e.g. `get trigger.pulse`.
    Get {
        group: String,
        /// Instance (decoder/math 1..4).
        #[arg(short = 'n', long)]
        instance: Option<u8>,
    },
    /// Change a settings group, e.g. `set trigger.pulse --activate when=GREater lower=2u`.
    Set {
        group: String,
        /// key=value pairs; numbers accept SI prefixes (2u, 1.5m, 10k).
        values: Vec<String>,
        #[arg(short = 'n', long)]
        instance: Option<u8>,
        /// Also activate the group (e.g. switch to this trigger type).
        #[arg(long)]
        activate: bool,
    },
    /// Firmware information and update procedure.
    Fw {
        #[command(subcommand)]
        cmd: commands::FwCmd,
    },
    /// Measure query latency and transfer rates (direct connection only).
    Bench {
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
    },
    /// Probe the instrument's SCPI dialect and write a report (direct connection only).
    Probe {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Verify guide commands on the instrument (budgeted: the firmware hangs after ~5
    /// unanswered queries per power cycle). Direct connection only.
    Verify {
        /// Groups to check (default: all). `--list` shows them.
        groups: Vec<String>,
        #[arg(long)]
        list: bool,
        /// After a power cycle: reset the unanswered-query budget.
        #[arg(long)]
        reset_budget: bool,
        /// Unanswered queries already spent in this power cycle (e.g. by earlier tests).
        #[arg(long)]
        assume_used: Option<u32>,
        /// Results file (merged with earlier runs).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Print the effective configuration as TOML.
    Config,
}

fn init_logging(verbose: u8, stderr_only: bool) {
    let level = match verbose {
        0 => "warn,mho954=info,mho_server=info",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into());
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    if stderr_only {
        builder.with_writer(std::io::stderr).init();
    } else {
        builder.init();
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // stdout belongs to the MCP protocol in `mcp` mode.
    init_logging(cli.verbose, true);
    let mut cfg = Config::load(cli.config.as_deref())?;
    if let Some(c) = &cli.connection {
        cfg.connection = c.clone();
    }

    if let Command::Gui { sim } = &cli.command {
        return commands::gui(&cli, cfg, *sim);
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(commands::run(cli, cfg))
}
