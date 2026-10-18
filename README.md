# mho954

Control of the **Rigol MHO954** (MHO900 series) on Linux, in Rust. The program provides:

- **Live view** (egui): channels in scope colours, draggable time and level cursors, zoom,
  context menu, measurements (also between the cursors), FFT (also over a section), function
  generator, Bode plot (live), counter/voltmeter, readouts, screenshot, CSV and RAW export, LAN
  settings
- **CLI** for all functions, measurements, captures, generator, Bode, backup/restore, LAN and an
  SCPI console
- **REST API** with OpenAPI documentation and a **WebSocket live stream**
- **MCP server** for LLM agents (Claude Code and others), over HTTP or stdio
- a **simulator** that reproduces the quirks of the real instrument

The connection runs over USB (own USBTMC implementation in user space, robust even over USB/IP)
or LAN/WLAN (raw SCPI port 5555). NI-VISA is not needed.
**Networking is optional:** the full feature set works over USB.

## Layout

```
crates/
  mho-scpi     transports: USBTMC in user space (nusb, default), kernel usbtmc, TCP:5555; framing, recovery
  mho-driver   typed MHO900 commands, capabilities (bandwidth, memory, options), settings tables,
               generator, Bode, logic analyzer, verification, simulator
  mho-dsp      measurements, FFT, Bode evaluation, decoder/peak tables, min/max decimation,
               CSV/NPY/VCD export
  mho-service  device thread, live loop and events, ScopeApi (local + HTTP client)
  mho-server   axum: REST /api/v1, WebSocket /api/v1/stream, MCP /mcp, docs /docs
  mho-gui      eframe/egui live view
  mho954       binary (CLI)
```

One process owns the instrument exclusively: the daemon (`mho954 serve`) or the GUI with its
embedded server. All other clients (CLI, MCP, further GUIs) go through its API. The CLI detects a
running daemon automatically, even while it is busy with a long operation.

## Installation

```bash
cargo install --path crates/mho954        # → ~/.cargo/bin/mho954
sudo contrib/install-udev.sh              # udev rule for USB access (group dialout)
```

The default transport `usb` needs write access to the scope's `/dev/bus/usb/BBB/DDD`. The rule in
`udev/62-rigol-usbip.rules` grants it to the group `dialout`; the user must be a member. While a
connection is open, the kernel driver `usbtmc` is detached and reattached afterwards
(`/dev/usbtmc0` reappears after ~15 ms).

Optional:
- `contrib/mho954.service` – systemd user unit for the daemon
- `contrib/mho954.desktop` – start menu entry for the GUI

## Quick start

```bash
mho954 idn                                 # identify the instrument (+ capabilities in one line)
mho954 caps                                # bandwidth (with upgrade), memory depth, generator, options
mho954 status                              # all settings (--json for JSON)
mho954 gui                                 # live view (starts an embedded server)
mho954 gui --sim                           # the same with the simulator

mho954 channel 1 --on --scale 0.5 --coupling dc --bwl 20M --probe 10
mho954 timebase --scale 200e-6 --offset 0
mho954 trigger --source CHAN1 --slope rising --level 1.5 --sweep normal --coupling dc
mho954 acquire                             # show acquisition settings
mho954 acquire --depth 500M --type average --averages 16   # change them (limits: see caps)
mho954 run | stop | single | auto | force | clear
mho954 measure VPP FREQ RTIM -s 1          # measurement on the scope (--list: all items)
mho954 measure RRDELAY -s 1 --source2 2    # measurements with two sources
mho954 measure --local -s 1                # locally from the screen waveform
mho954 fft -s 1 -n 5 --window hann         # spectrum, computed locally (rectangular|hann|blackman|flattop)
mho954 screenshot -o shot.png
mho954 capture -s 1 -o screen.csv          # screen waveform (.csv | .npy + .json | .json)
mho954 stop && mho954 capture -s 1 --raw -o cap.npy   # full memory, + cap.json
mho954 scpi ':ACQ:SRAT?'                   # single command (blocks are detected)
mho954 scpi                                # interactive console
mho954 backup                              # IDN, status, options, LAN, live config, setup, screenshot
mho954 restore ~/mho954-backups/<date>/setup.bin
mho954 net                                 # LAN settings (RJ45)
mho954 net set --static --ip 192.168.1.50 --mask 255.255.255.0 --gateway 192.168.1.1
mho954 net set --dhcp --mdns true --hostname mho954
mho954 options                             # options; options install 'MHO900-…@…'
mho954 fw status | fw update               # firmware version / update procedure
mho954 bench | probe -o report.json        # latency/throughput, SCPI dialect (no daemon running)
mho954 config                              # effective configuration as TOML

mho954 groups trigger --params             # settings groups (trigger types, decoders, math, …)
mho954 get trigger.pulse                   # read a group (-n for instances, e.g. decoders 1..4)
mho954 set trigger.pulse --activate when=less upper=2u   # write; --activate selects the type
mho954 stats VPP -s 1 --reset              # measurement statistics (current/mean/min/max/σ/count)
mho954 counter | dvm                       # frequency counter, voltmeter (first `set counter enable=true`)
mho954 set bus.rs232 -n 1 --activate baud=115200 tx=CHAN2 tx_threshold=1.6
mho954 decode 1                            # decoder event table (--csv, -o bus1.csv)
mho954 set math -n 1 --activate operator=mult source1=CHAN1 source2=CHAN2
mho954 set math.fft -n 2 --activate source=CHAN2 peak_search=true peaks=5 peak_threshold=-60
mho954 peaks 2                             # peak table of the scope FFT, e.g. " 1  1.025 kHz  -6.802dBV"
mho954 set cursor.manual --activate ax=-100u bx=300u && mho954 read cursor.manual
mho954 read --list                         # readouts: cursor, mask, search, histogram, …
mho954 read histogram --enable             # switch the mode on if needed, then read
mho954 do save.setup C:/bench1.stp         # file on the scope (C:/ internal, D:/ USB stick)
mho954 do --list                           # commands with preconditions; `do reset --yes` = *RST
mho954 logic --bus 4 -o capture.vcd        # D0..D15 (logic probe) as VCD for PulseView

mho954 afg 1 --func sin --freq 1k --ampl 1 --load 50 --on   # generator GI (AFG option)
mho954 afg 2 --func squ --freq 10k --duty 25 --mod am --depth 50 --mod-freq 100 --on
mho954 afg 1 --off                         # output off; `afg 1` shows the state
mho954 do afg.align_phase                  # restart GI/GII in phase
mho954 do afg1.load_arb D:/wave.csv        # arb file from the scope's USB stick
mho954 bode --start 100 --stop 1M --dut-in 1 --dut-out 2 -o bode.csv   # software sweep (AFG)
mho954 bode --start 100 --stop 1M --averages 4 --load 50 --settle 200  # averaged, 50 Ω load
mho954 bode --start 100 --stop 80M --follow 2   # without a T piece: GI→CH1 against GII→CH2
mho954 bode --native --start 100 --stop 1M -o bode.png                  # the scope's Bode (image)
mho954 bode --native --keep --start 100 --stop 1M                       # plot stays on the display
mho954 set bus.flexray -n 1 --activate source=CHAN1 baud=10000000      # options FlexA/AUDIOA/AEROA
mho954 set trigger.i2s --activate clock=CHAN1 word_select=CHAN2 data_source=CHAN3
mho954 verify --list                       # check commands one by one on the instrument (see below)
```

`mho954 <command> --help` describes all options. Global options: `-c/--connection`, `--url`,
`--direct`, `--config`, `-v`/`-vv`/`-vvv`.

| Connection | Meaning |
|---|---|
| `usb` (default), `usb:SERIAL` | USBTMC in user space; only MHO900 (PID 0452), otherwise by serial number |
| `usbtmc`, `/dev/usbtmcN` | Linux kernel driver (less robust over USB/IP) |
| `tcp:HOST[:PORT]` | LAN/WLAN, raw SCPI on port 5555 |
| `sim` | simulator, see `MHO954_SIM` |

Environment variables: `MHO954_CONFIG`, `MHO954_CONNECTION`, `MHO954_URL`, `MHO954_SIM`,
`MHO954_ALLOW_ANY_MODEL=1` (allow other Rigol models), `RUST_LOG`.

`MHO954_SIM` (comma-separated): the default is an upgraded MHO954 (all options of the test scope,
generator, logic probe, signal on all channels; GI on CH1, GII on CH2; without a GII output, a
10 kHz RC low-pass sits behind GI on CH2 as long as GI outputs a sine).

| Option | Effect |
|---|---|
| `latency=25` | response time per query in ms |
| `nosignal=1+4` | channels without data (0xFFFF) |
| `fullidn` | full firmware version in `*IDN?` |
| `strict` | unverified queries are rejected as on the instrument |
| `noopt` | no options (state before the upgrade) |
| `noafg` | no generator |
| `noprobe` | no logic probe (D0..D15 as decoder source → -200) |
| `hang_after=5` | unknown queries after which the simulated firmware hangs |

**WLAN:** There are no SCPI commands for the WLAN dongle. WLAN is set up on the scope. After that,
`-c tcp:<IP>` with the scope's WLAN IP works as usual.

## Capabilities and options

`mho954 caps` (REST `/api/v1/capabilities`, MCP `get_capabilities`) summarizes what the
instrument can do. It derives this from `*IDN?`, the options and `:SYST:MOD?`:

- **Bandwidth:** MHO934 350 MHz, MHO954 500 MHz, MHO984 800 MHz, each at most 400 MHz with
  3–4 channels (MHO934: 350 MHz). With BWU05T08 or BWU03T08 it is 800 MHz with 1–2 channels, with
  BWU03T05 (MHO934) 500 MHz. After a bandwidth upgrade, `*IDN?` still reports the original model
  (here `MHO954`).
- **Memory depth:** at most 100M/50M/25M points with 1/2/3–4 channels, with RLU05 500M/250M/125M.
  The program rejects deeper values up front with a clear message; the GUI does not offer them.
- **Generator:** present if `:SYST:MOD?` reports it **or** an AFG license is active and the
  instrument has answered `:SOUR1:OUTP:STAT?`. On the upgraded test scope, the DG field of
  `:SYST:MOD?` stays at 0 although the generator works.
- **Protocol options:** CAN FD (AUTOA), FlexRay (FlexA), I²S (AUDIOA), MIL-STD-1553 (AEROA).
  Their groups (`bus.canfd`, `bus.flexray`, `bus.i2s`, `bus.m1553`, `trigger.flexray`,
  `trigger.i2s`, `trigger.m1553`) check the license before reading and writing; the GUI hides them
  without a license. There are no SCPI commands for the CAN FD trigger. The I²S clock threshold
  shares its value with the SPI clock threshold (key `CLK`).
- EMBD, COM and AUTO are standard on the MHO900 and are listed as `standard`, not queried.

## Function generator

Two channels GI/GII (option AFG50/AFG100). `mho954 afg`, REST `PUT /api/v1/generator/{n}`, MCP
`set_generator`, GUI "Function generator (AFG)", or the groups `afg`/`afg.modulation`.

- Waveforms SIN, SQU, RAMP, NOIS, DC, ARB, EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC; frequency or
  period, amplitude, offset or high/low level, phase, duty cycle, symmetry, load (High-Z/50 Ω),
  modulation AM/FM/PM.
- **Limits** (checked before sending): sine up to 100 MHz (AFG100) or 50 MHz (AFG50), square,
  arb and built-in waveforms up to 20/10 MHz, ramp up to 2/1 MHz, from 2 mHz (AFG50: 20 mHz).
  Amplitude up to 10 Vpp into 50 Ω (5 Vpp above 50 MHz), twice that into High-Z; offset at most
  ±(Vmax − Vpp)/2.
- The output is switched on last (and off first); the whole change runs as one device job, no
  other client can get in between.
- `do afg.align_phase` restarts GI and GII in phase. `do afg1.load_arb D:/x.csv` loads an arb file
  that is on the scope or its USB stick (there is no SCPI upload).

## Bode plot

Two ways, both need the generator:

- **Software sweep** (`mho954 bode`, REST `POST /api/v1/bode`, MCP `bode_sweep`, GUI): per
  frequency, the generator is set to sine, one single acquisition is taken, and amplitude and phase
  of both channels are computed by a single-bin DFT from the same acquisition (so the phase does
  not depend on the trigger). The channel scales start to match the generator amplitude and are
  readjusted per point. Optionally complex-averaged (`--averages`), with load (`--load`) and
  settle time (`--settle`). Afterwards, timebase, scales, offsets, trigger and run state are
  restored and the outputs switched off. Result: gain/phase per point, CSV with summary (−3 dB,
  0 dB crossing, phase and gain margin, unwrapped phase). Every sweep goes live over the
  WebSocket; the GUI draws it no matter who started it.
- **Without a T piece** (`--follow 2`): GII runs at the same frequency and is restarted in phase
  with GI at every step; CH2/CH1 is measured. This compares the paths GI→CH1 and GII→CH2 (on the
  test scope within 0.1 dB up to 10 MHz, constant delay difference 6.5 ns), or measures a device
  between GI and CH2 with GII as reference on CH1.
- **The scope's Bode** (`--native`, REST `/api/v1/bode/native`, MCP `bode_native`): the scope
  measures by itself (start 10 Hz–3 MHz, stop 100 Hz–30 MHz, start ≤ stop/10, 10–100 points per
  decade). There are no measured values over SCPI; the result is a screenshot. It only drives GI
  and sets the two measurement channels (default CH1/CH2) to AC, 1 mV/div and 20 MHz; afterwards
  the channels are restored, and with `--keep` the plot stays on the display.

## RAW readout and deep memory

`capture --raw` (REST `?mode=raw`, GUI "RAW to .npy") reads the whole memory of a stopped scope
in blocks; `.npy` is written without an intermediate copy.

- The memory depth is an upper limit: at a maximum of 4 GSa/s, the memory only fills at slow
  timebases. At 200 µs/div it is ≈10M points; the full 500M need ≥ 12.5 ms/div.
- Throughput on the test scope: USB ≈ 1.2–1.4 MB/s, LAN ≈ 0.9 MB/s. 500M points (1 GB) take
  ≈ 12 minutes over USB and use about 1 GB of RAM.
- The CLI writes CSV or JSON up to 10M points and rejects larger datasets up front; above that,
  `.npy` (ADC codes + preamble in `.json`).

## GUI

`mho954 gui` starts the live view with an embedded server (or uses a running daemon). It
remembers its view settings in `~/.config/mho954/gui.json`, falls back to a free port if 8954 is
taken, and `gui --sim` always uses a free port so the CLI cannot control the simulator by
accident.

- **Header:** model, bandwidth, memory depth, generator and protocol options, red indicator
  `GI ON`/`GII ON`, connection, trigger status, frame rate, progress bar for long operations.
- **Scope plot:**
  - Left click sets cursor A. Cursors (A/B in time, Y1/Y2 in level) can be dragged with the mouse.
    Middle click clears all.
  - Right-drag zooms to a rectangle, the mouse wheel pans, Ctrl+wheel zooms, double click resets
    the view. The `view` bar has reset / fit Y / X± / Y±.
  - **Context menu:** set or clear cursors here; zoom to A–B, zoom in/out, fit vertically, reset;
    "Measure A-B" (local measurements over that range only, as rows `CHx [A-B]`); FFT over A–B or
    the visible range (shaded orange); on the scope: trigger level here, center time here, take the
    visible range as timebase (more detail than zooming into the 1000 screen points), screenshot.
  - The cursor readout shows values per channel, Δt, 1/Δt and ΔY; in the div view it converts ΔY
    to volts per channel.
  - FFT: logarithmic frequency axis, automatic Y scaling, its own context menu, peak table of the
    scope FFT.
- **Controls on the right:** Run/Stop/Single/Auto/Force/Clear; channels (V/div, offset, coupling,
  impedance, bandwidth limit, probe, invert); timebase; trigger with type selection (only
  available types); acquisition (depth only up to the limit, type, number of averages); capture
  (screenshot, CSV, "Stop + RAW to .npy"); live frame rate.
- **Advanced settings:** all settings groups with value ranges; groups whose option or hardware is
  missing are not offered.
- **Readouts & commands:** readouts with "Enable and read" (switches the mode on) and
  "Settings…"; commands show their precondition, files default to `C:/`.
- **Function generator (AFG):** GI/GII, waveform, fields per waveform, load, modulation, Apply
  (output unchanged), Output ON/OFF (red when on), Align phase, load arb file, Read.
- **Bode plot (AFG):** software sweep or the scope's Bode ("keep on scope screen"), load,
  averaging, "other generator in step" (= `--follow`). The Bode window opens by itself on every
  sweep, draws live, shows the previous sweep in grey, the summary and a CSV button.
- **Counter & DVM:** on/off, source, value every second; "Settings…" opens the group.
- **Network (RJ45)**, **System** (save/load setup, options, install a purchased license).
- At the bottom: measurement table (local and on the scope, statistics), decoder table with
  labels in the plot, Logic (VCD) with logic analyzer.

## Daemon, REST and WebSocket

```bash
mho954 serve                      # 127.0.0.1:8954 (--bind ADDRESS for others)
mho954 serve --allow-raw-scpi     # additionally POST /api/v1/scpi
```

Ctrl-C and SIGTERM stop the daemon cleanly: a running transfer is read to the end before the
instrument is released.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/v1/info`, `/api/v1/status` | identification, complete state |
| GET/PUT | `/api/v1/channels/{n}`, `/timebase`, `/trigger`, `/acquire` | read/change (only the fields given) |
| POST | `/api/v1/actions/{run,stop,single,autoscale,clear,force}` | run control |
| GET/POST | `/api/v1/measure?items=VPP,FREQ&source=CHAN1[&source2=CHAN2]` | measurements on the scope |
| GET | `/api/v1/measure/items` | catalogue of measurement items |
| GET | `/api/v1/measure/local?source=CHAN1` | local measurements (`has_data`) |
| GET | `/api/v1/fft?source=CHAN1&window=flattop&peaks=5&raw=false&full=false` | spectrum |
| GET | `/api/v1/waveform/{src}?mode=norm\|raw&format=json\|csv\|npy\|bin` | waveform (default json; npy with header `X-Preamble`) |
| GET | `/api/v1/screenshot.png` | screen |
| GET/POST | `/api/v1/options`, `/api/v1/options/install` | options, install a purchased license |
| GET/PUT | `/api/v1/setup` | save/load the setup blob |
| GET/PUT | `/api/v1/network` | LAN: DHCP/Auto-IP/static, IP, mask, gateway, DNS, mDNS, hostname |
| GET/PUT | `/api/v1/live` | live loop: fps, measurement list (max. 10) |
| GET | `/api/v1/groups`, GET `/api/v1/groups/{id}?instance=n`, PUT `…?instance=n&activate=true` | settings groups: catalogue, read, write |
| GET | `/api/v1/measure/stats?item=VPP&source=CHAN1` | measurement statistics |
| POST | `/api/v1/measure/actions/{reset_statistics,clear_all,clear_totalizer}` | reset statistics/measurements/totalizer |
| GET | `/api/v1/counter`, `/api/v1/dvm` | counter, voltmeter (409 while off) |
| GET | `/api/v1/capabilities`, `/api/v1/modules` | bandwidth, memory limits, generator, protocol options; LA/AFG |
| PUT | `/api/v1/generator/{n}` `{function, frequency, amplitude, offset, load, modulation, …, output}` | generator GI/GII (one device job, output last) |
| GET | `/api/v1/bus/{n}/data?format=json\|csv` | decoder event table |
| GET | `/api/v1/math/{n}/peaks` | peak table of the scope FFT |
| GET | `/api/v1/readouts`, `/api/v1/readouts/{id}?enable=true` | readouts (cursor, mask, search, histogram, memory status, arb files); `enable` switches the mode on |
| GET/POST | `/api/v1/commands`, `/api/v1/commands/{id}` `{"path","confirm"}` | save/load, resets, record, references (`path` = number), generator; preconditions in the catalogue; `reset` only with `confirm` |
| GET | `/api/v1/logic/{bus}?format=json\|vcd` | digital channels via the parallel decoder |
| POST | `/api/v1/bode` `{start, stop, points, input, output, amplitude, generator, averages, load, follow, settle_ms}` | software Bode sweep (AFG option) |
| POST | `/api/v1/bode/native` `{start, stop, points_per_decade, input, output, amplitude, linear, timeout_s, keep_on_screen}` | the scope's Bode plot, result as PNG |
| POST | `/api/v1/scpi` | raw SCPI (only with `--allow-raw-scpi`) |
| WS | `/api/v1/stream` | live frames (binary) and text messages, see below |

Errors come as JSON `{kind, message}` with an HTTP status: 400 invalid, 403 forbidden, 409 wrong
state (e.g. RAW while running, mode off), 422 instrument error, 502 protocol error, 503 scope not
reachable, 504 timeout.

Docs: <http://127.0.0.1:8954/docs> (also `/`), OpenAPI: `/api/v1/openapi.json`.

**WebSocket frames:** `[u32 LE header length][JSON header][u16 LE samples of all traces]`. The
header contains `seq`, `timestamp_ms`, `trigger_status`, `fps` and `traces[]` with `source`,
`preamble` and `len`. Volts = `(raw − y_origin − y_reference) × y_increment`.
Text messages: `{"type":"measurements",...}`, `{"type":"error",...}`,
`{"type":"progress",...}` (long operations, `total = 0` at the end) and `{"type":"bode",…}`
(after every point of a Bode sweep, no matter who started it; new connections get the last
sweep).

**Security:**
- **Loopback bind (default):**
  - Requests with a foreign `Host` are rejected (protection against DNS rebinding).
  - So are writing requests and WebSocket connections with a foreign `Origin` (protection against websites in the browser).
- **LAN bind** (`bind = "0.0.0.0:8954"`): `api_token` must be set. Without a token, the server does not start. All clients send it as `Authorization: Bearer <token>`; CLI and GUI take it from the config.

## MCP

```bash
# HTTP (the daemon or the GUI must be running):
claude mcp add --transport http mho954 http://127.0.0.1:8954/mcp
# or stdio (uses the daemon if it is running, otherwise the instrument directly):
claude mcp add mho954 -- mho954 mcp
```

The server instructions ask the model to read `get_status` and `get_capabilities` first and to ask
before switching on a generator output, autoscale, reset or network changes.

| Tool | Function |
|---|---|
| `identify`, `get_status`, `get_capabilities` | identification, state, capabilities (bandwidth, memory limits, generator, options) |
| `configure_channel`, `configure_timebase`, `configure_trigger`, `configure_acquisition` | change settings |
| `run_control` | run, stop, single, autoscale, clear, force |
| `measure`, `list_measurement_items` | measurements on the scope |
| `analyze_waveform` | local measurements (`has_data`) |
| `get_waveform` | decimated samples; `raw=true` full memory; `save_as` saves CSV/NPY in the capture folder (never overwriting) |
| `fft_peaks` | spectrum peaks (computed locally) |
| `list_settings_groups`, `get_settings`, `configure_settings` | all settings groups (trigger types, decoders incl. options, math, counter, DVM, generator, Bode, …) |
| `measurement_statistics`, `measurement_action` | measurement statistics, reset |
| `read_counter`, `read_dvm` | frequency counter, voltmeter |
| `decode_bus` | decoder events |
| `scope_fft_peaks` | peak table of the scope FFT |
| `list_readouts`, `read_values` | readouts with availability; read, `enable` switches the mode on |
| `list_scope_commands`, `run_scope_command` | commands with preconditions; save/load/resets/generator (`reset` only after asking) |
| `logic_capture` | digital channels D0..D15 (optionally saved as VCD) |
| `set_generator` | set up generator GI/GII, output on/off |
| `bode_sweep` | frequency response with the built-in generator (averages, load, follow) |
| `bode_native` | the scope's Bode plot as image (`keep_on_screen`) |
| `screenshot` | image as MCP image |
| `list_options` | options |
| `get_network`, `configure_network` | LAN settings (changes only after asking) |
| `raw_scpi` | only with `allow_raw_scpi` |

## Configuration

`~/.config/mho954/config.toml` (show with `mho954 config`; unknown keys are an error):

```toml
connection = "usb"            # usb[:SERIAL] | usbtmc | /dev/usbtmc0 | tcp:HOST[:5555] | sim
bind = "127.0.0.1:8954"
allow_raw_scpi = false
# api_token = "…"             # required if bind is not loopback
capture_dir = "/home/philipp/mho954-captures"
backup_dir = "/home/philipp/mho954-backups"
latest_firmware = "00.01.00.00.26"

[live]
enabled = true
max_fps = 30.0                # ≤ 100
stopped_fps = 2.0             # ≤ max_fps
measure_interval_ms = 1000    # ≥ 200
preamble_refresh_ms = 1000    # ≥ 100
status_every = 3              # :TRIG:STAT? every n-th frame
measurements = [{ item = "VPP", source = "CHAN1" }, { item = "FREQ", source = "CHAN1" }]
```

## Firmware and options

- `mho954 fw status` compares the installed version with `latest_firmware`. Via `*IDN?` the scope
  only reports `00.01.00`; the full version is on the instrument under Utility → About.
- `mho954 fw update` shows the procedure: backup, `.GEL` from rigol.com onto a FAT32 stick,
  update on the instrument. There is no update command over SCPI.
- `mho954 options` lists the options.
- `mho954 options install 'MHO900-BWU05T08@…'` (GUI: System → Install) installs a **purchased**
  license. The license is generated on rigol.com → License Activation from the key and the serial
  number.
- Bypassing the license check (keygen, `vendor.bin` patch, patched firmware) is deliberately
  **not** included.

## Settings groups and verification

Trigger types, measurement setup, counter, voltmeter, the decoders `bus.*` (1–4), the math
channels `math*` (1–4), channel and timebase extras (label, deskew, zoom, roll, XY), autoset,
display, cursor, save, histogram, mask test, search, record, references, logic analyzer,
generator, Bode and the option protocols are described by a table in
`crates/mho-driver/src/params.rs`. Readouts and one-off commands are in `ops.rs`. From these come
validation, CLI (`groups/get/set`), REST, MCP, the GUI panel "Advanced settings" and the checklist
for `mho954 verify`.

Because the scope does not answer unknown queries and hangs after about five of them:
- **Writing** always works. Errors end up in the error queue and are reported.
- **Reading** a group, decoder tables, FFT peaks, math traces in the live view and the DVM are
  only available once the queries are confirmed on the instrument. As of 2026-10-18 (after the
  upgrade), all groups and readouts are confirmed, including generator, Bode and the option
  protocols.
- **Readouts** (cursor, mask, search, histogram) need their mode on the scope;
  `mho954 read histogram --enable` (GUI: "Enable and read", REST `?enable=true`, MCP
  `enable: true`) switches it on if needed. Without that, the message names the group.
- **Commands** with a precondition check it first: `histogram.save/reset` (histogram on and with
  data), `mask.create/reset`, `save.mask` (mask test on), `record.*` (recording on). Files: `C:/`
  is the internal memory, `D:/` the USB stick in the scope.
- **`mho954 verify [groups]`** (direct only, without a running daemon) sends every not yet
  confirmed query one by one, followed by `*IDN?`; write commands always run. Results land in
  `~/.local/state/mho954/verified.json` (`-o` for another file) and are then carried over into
  `docs/verified-commands.json`; that is compiled in on the next build.
  - Budget: after three unanswered queries it stops and asks for a restart of the scope; after
    that, `mho954 verify --reset-budget`. `--assume-used N` counts ones already used up.
  - If a query stays unanswered before any query of the same group has been answered in this run
    (already confirmed ones don't count), the remaining queries of the group and its subgroups are
    skipped (the group is probably missing entirely). If one was already answered, a later failure
    counts as a single wrong command.
  - Without group arguments, `verify` checks everything. With groups it includes their subgroups,
    but the fixed probes (e.g. `bus.data`, `math.peaks`, `dvm.value`, `system.dg`) only when named
    exactly; `bus.data` and `math.peaks` briefly change the scope's settings for that.
- The simulator accepts unverified queries so that everything can be tested without an
  instrument; `MHO954_SIM=strict` behaves like the instrument.

## Logic analyzer

The MHO954 rejects `:WAV:SOUR D0`. `logic` therefore sets up a decoder as a 16-bit parallel bus
(D15..D0, no clock, one event per change), reads its event table and restores the decoder
afterwards. The time resolution matches the decoder's sampling. The bit order (D15 = MSB with
`D15D0`) is still to be confirmed on the instrument. Thresholds and channels are in the groups
`la` and `la.channel`. Without a logic probe (PLA2216) plugged in, the scope rejects D15..D0 as a
decoder source; `logic` reports that.

## Known quirks of the MHO954

Details in `docs/scpi-mho900.md` (German).

- **Unknown SCPI queries:** the scope does not answer them, and then nothing at all until a USBTMC
  device clear arrives; the transport sends it automatically. After around five unknown queries
  since power-on, the firmware hangs permanently and only a restart helps. The driver checks all
  parameters up front. Raw SCPI locks itself as soon as the scope stops answering.
- **Short forms:** the short form derived from the guide's capital letters is not always right
  (`:BOD:PHASE:ENAB?` stays unanswered, `:BODeplot:PHASEcurve:ENABle?` works). New commands are
  therefore in the tables in their long form. `:BUS<n>:THR? I2SClk` is write-only.
- **First measurement:** a newly created measurement returns `9.9E37` on the first read; the
  driver asks once more.
- **Channels without data:** they return `0xFFFF` throughout. Program, API and GUI mark them as
  "no data" (`has_data: false`).
- **Single acquisition:** right after `:SING`, `:TRIG:STAT?` briefly still reports the old STOP,
  and at slow timebases the preamble briefly reports 0 points after STOP. The Bode sweep therefore
  waits out the acquisition time and rereads incomplete datasets.
- **RAW preamble:** in RAW mode it reports the configured read window, not the dataset (up to
  500M; the guide says 50M); the readout opens the window to the memory depth first.
- **Histogram:** for the vertical histogram, firmware 00.01.00 reports a wrong `Sigma` (3 mV for
  a 1 Vpp sine); use `ACRMS` for the spread.
- **Counter:** when off, `:COUN:CURR?` answers 0; the program then reports "off".
- **FFT peaks:** the threshold `peak_threshold` is in the FFT unit; the factory value 5.5 dBV
  hides the peaks of most signals (empty table); `-60` suits dB. When the unit changes, the scope
  converts threshold and excursion, so groups write in table order (unit before threshold). dBV
  refers to the RMS value; with the flat-top window the `VRMS` values are too small by a factor of
  4.64 (in dB they are right). The frequencies lie on the bin grid (up to ~80 Hz off at a 20 kHz
  span).
- **USB framing:** the scope sends no USBTMC padding bytes, and large responses come in several
  transfers. An open response is read to the end before every new command; a device clear in the
  middle of a response used to block the scope.
- **Kernel driver:** `ABORT_BULK_*` hangs in the kernel under USB/IP; this only affects
  `-c usbtmc`.
- **TCP:** there is no device clear; the transport reconnects instead. So check unknown queries
  over USB only.
- **Status query:** `mho954 status` needs about 40 queries (≈1 s). The GUI loads it every 15 s or
  when the displayed channels change.

## Development

```bash
cargo test --workspace                                   # incl. simulator and end-to-end tests
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
mho954 -c sim serve                                      # API without hardware
```

Hardware tests (`crates/mho-driver/tests/hw.rs`, verified commands only, `*IDN?` after every
step):

```bash
MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1
```

| Variable | Meaning |
|---|---|
| `MHO954_HW=1` | enable hardware tests |
| `MHO954_HW_CONN=tcp:192.168.1.109` | connection (default `usb`) |
| `MHO954_HW_AFG=1` | generator test; wiring GI → CH1, GII → CH2 (1 MΩ) |
| `MHO954_HW_FFT_CH=4` | input carrying GI for the FFT peak test `fft_peak_table_from_square` (default 1) |
| `MHO954_HW_BODE=1` | Bode test; GI through a T piece to CH1 and CH2 (flat frequency response) |

CI (`.forgejo/workflows/ci.yml`) runs fmt, clippy and tests on the host runner `apps-ws`.
