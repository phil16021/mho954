//! `mho954` — command-line entry point.

mod commands;
mod config;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser)]
#[command(name = "mho954", version, about = "Control the Rigol MHO954 oscilloscope: live view, REST API, MCP server, CLI")]
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
    /// Show the full scope state.
    Status {
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
        /// Compute locally from the screen waveform instead of asking the scope.
        #[arg(long)]
        local: bool,
        /// List available items.
        #[arg(long)]
        list: bool,
    },
    /// Save a screenshot.
    Screenshot {
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
    /// Spectrum peaks of a channel.
    Fft {
        #[arg(short, long, default_value = "CHAN1")]
        source: String,
        #[arg(short = 'n', long, default_value_t = 5)]
        peaks: usize,
        #[arg(long)]
        raw: bool,
    },
    /// Software options.
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
    /// LAN (RJ45) settings: show, or change with `net set ...` (WLAN has no SCPI interface).
    Net {
        #[command(subcommand)]
        cmd: Option<commands::NetCmd>,
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
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
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
