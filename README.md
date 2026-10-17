# mho954

Steuerung des **Rigol MHO954** (MHO900-Serie) unter Linux, in Rust. Das Programm bietet:

- **Live-Ansicht** (egui): Kanäle in Scope-Farben, Cursor, Trigger-Marker, lokale und
  scope-seitige Messungen, FFT, Screenshot, CSV- und RAW-Export
- **CLI** für alle Grundfunktionen, Messungen, Captures, Backup und eine SCPI-Konsole
- **REST-API** mit OpenAPI-Dokumentation und einem **WebSocket-Livestream**
- **MCP-Server** für LLM-Agenten (Claude Code u. a.), per HTTP oder stdio
- einen **Simulator**, damit alles auch ohne Gerät läuft

Verbindung über USB (Linux-Kernel-Treiber `usbtmc`) oder LAN (Raw-SCPI-Port 5555).
NI-VISA wird nicht benötigt.

## Aufbau

```
crates/
  mho-scpi     Transport: USBTMC (/dev/usbtmcN), TCP:5555, IEEE-488.2-Blöcke, Recovery
  mho-driver   typisierte MHO900-Befehle, Modelle, Simulator
  mho-dsp      Messungen, FFT, Min/Max-Dezimierung, CSV/NPY-Export
  mho-service  Geräte-Thread, Live-Loop, ScopeApi (lokal + HTTP-Client)
  mho-server   axum: REST /api/v1, WebSocket /api/v1/stream, MCP /mcp, Doku /docs
  mho-gui      eframe/egui-Live-Ansicht
  mho954       Binary (CLI)
```

Ein Prozess besitzt das Gerät exklusiv: der Daemon (`mho954 serve`) oder die GUI mit
eingebettetem Server. Alle anderen Clients (CLI, MCP, weitere GUIs) gehen über dessen API.
Die CLI erkennt einen laufenden Daemon automatisch.

## Installation

```bash
cargo install --path crates/mho954        # → ~/.cargo/bin/mho954
```

Der USB-Zugriff setzt eine udev-Regel voraus (Vorlage: `udev/62-rigol-usbip.rules`, Gruppe
`dialout`). Der Kernel-Treiber `usbtmc` muss geladen sein.

## Schnellstart

```bash
mho954 idn                                 # Gerät identifizieren
mho954 status                              # alle Einstellungen
mho954 gui                                 # Live-Ansicht (startet eingebetteten Server)
mho954 gui --sim                           # dasselbe mit Simulator

mho954 channel 1 --on --scale 0.5 --coupling DC
mho954 timebase --scale 200e-6
mho954 trigger --source CHAN1 --slope positive --level 1.5 --sweep normal
mho954 measure VPP FREQ RTIM -s 1          # Messung am Scope
mho954 measure --local -s 1                # lokal aus der Bildschirm-Wellenform
mho954 fft -s 1 -n 5
mho954 screenshot -o shot.png
mho954 single && mho954 capture -s 1 --raw -o cap.npy   # voller Speicher, + cap.json
mho954 scpi ':ACQ:SRAT?'                   # einzelner Befehl
mho954 scpi                                # interaktive Konsole
mho954 backup                              # IDN, Status, Optionen, Setup-Blob, Screenshot
mho954 net                                 # LAN-Einstellungen (RJ45)
mho954 net set --static --ip 192.168.1.50 --mask 255.255.255.0 --gateway 192.168.1.1
mho954 net set --dhcp --mdns true --hostname mho954
mho954 bench                               # Latenz/Durchsatz (ohne laufenden Daemon)
mho954 probe -o report.json                # SCPI-Dialekt prüfen
```

`-c usb` (Standard, USBTMC im Userspace), `-c usbtmc` (Kernel-Treiber), `-c tcp:192.168.1.20`
(LAN/WLAN, Port 5555) oder `-c sim` wählen die Verbindung explizit.

**Netzwerk ist optional:** Der volle Funktionsumfang läuft über USB. Dazu gehören GUI, REST, MCP,
RAW, Screenshots und auch die LAN-Konfiguration. LAN/WLAN ist nur ein alternativer Weg zum Scope.

**WLAN:** Für den WLAN-Dongle gibt es laut Programming Guide keine SCPI-Befehle. Das WLAN wird am
Scope eingerichtet. Mit der WLAN-IP des Scopes funktioniert danach `-c tcp:<IP>` ganz normal.

## Daemon, REST und WebSocket

```bash
mho954 serve                      # 127.0.0.1:8954
mho954 serve --allow-raw-scpi     # zusätzlich POST /api/v1/scpi
```

Eine systemd-User-Unit liegt in `contrib/mho954.service`.

| Methode | Pfad | Zweck |
|---|---|---|
| GET | `/api/v1/info`, `/api/v1/status` | Kennung, kompletter Zustand |
| GET/PUT | `/api/v1/channels/{n}`, `/timebase`, `/trigger`, `/acquire` | lesen/ändern (nur gesetzte Felder) |
| POST | `/api/v1/actions/{run,stop,single,autoscale,clear,force}` | Bedienung |
| GET/POST | `/api/v1/measure?items=VPP,FREQ&source=CHAN1` | Messungen am Scope |
| GET | `/api/v1/measure/local?source=CHAN1` | lokale Messungen |
| GET | `/api/v1/fft?source=CHAN1&window=flattop&peaks=5` | Spektrum-Peaks |
| GET | `/api/v1/waveform/{src}?mode=norm\|raw&format=json\|csv\|npy\|bin` | Wellenform |
| GET | `/api/v1/screenshot.png` | Bildschirm |
| GET/POST | `/api/v1/options`, `/api/v1/options/install` | Optionen, gekaufte Lizenz installieren |
| GET/PUT | `/api/v1/setup` | Setup-Blob sichern/laden |
| GET/PUT | `/api/v1/live` | Live-Loop: fps, Messliste |
| GET/PUT | `/api/v1/network` | LAN (RJ45): DHCP/Auto-IP/statisch, IP, Maske, Gateway, DNS, mDNS, Hostname |
| WS | `/api/v1/stream` | Live-Frames (binär) + Messwerte (JSON) |

Doku: <http://127.0.0.1:8954/docs>, OpenAPI: `/api/v1/openapi.json`.

**WebSocket-Frames:** `[u32 LE Headerlänge][JSON-Header][u16 LE Samples aller Spuren]`. Der
Header enthält `seq`, `timestamp_ms`, `trigger_status`, `fps` und `traces[]` mit `source`,
`preamble` und `len`. Volt = `(raw − y_origin − y_reference) × y_increment`.
Text-Nachrichten: `{"type":"measurements",...}` oder `{"type":"error",...}`.

Die API hat keine Authentifizierung. Bind daher standardmäßig nur auf Loopback; LAN nur
bewusst per `bind` in der Config.

## MCP

```bash
# HTTP (Daemon oder GUI muss laufen):
claude mcp add --transport http mho954 http://127.0.0.1:8954/mcp
# oder stdio (nutzt den Daemon, falls er läuft, sonst direkt das Gerät):
claude mcp add mho954 -- mho954 mcp
```

Verfügbare Tools:

| Tool | Funktion |
|---|---|
| `identify`, `get_status` | Gerätekennung und Zustand |
| `configure_channel`, `configure_timebase`, `configure_trigger`, `configure_acquisition` | Einstellungen ändern |
| `run_control` | run, stop, single, autoscale, clear, force |
| `measure`, `list_measurement_items` | Messungen am Scope |
| `analyze_waveform` | lokale Messungen |
| `get_waveform` | dezimierte Samples; `save_as` speichert CSV/NPY im Capture-Ordner |
| `fft_peaks` | Spektrum-Peaks |
| `screenshot` | Bild als MCP-Image |
| `list_options` | Optionen |
| `get_network`, `configure_network` | LAN-Einstellungen (Änderungen nur nach Rückfrage) |
| `raw_scpi` | nur mit `allow_raw_scpi` |

## Konfiguration

`~/.config/mho954/config.toml` (anzeigen mit `mho954 config`):

```toml
connection = "usbtmc"          # usbtmc | /dev/usbtmc0 | tcp:HOST[:5555] | sim
bind = "127.0.0.1:8954"
allow_raw_scpi = false
capture_dir = "/home/philipp/mho954-captures"
backup_dir = "/home/philipp/mho954-backups"
latest_firmware = "00.01.00.00.26"

[live]
max_fps = 30.0
stopped_fps = 2.0
measure_interval_ms = 1000
measurements = [{ item = "VPP", source = "CHAN1" }, { item = "FREQ", source = "CHAN1" }]
```

## Firmware und Optionen

- `mho954 fw status` vergleicht die installierte Version mit `latest_firmware`.
- `mho954 fw update` zeigt den Ablauf: Backup, `.GEL` von rigol.com auf einen FAT32-Stick,
  Update am Gerät. Per SCPI gibt es keinen Update-Befehl.
- `mho954 options` listet die Optionen.
- `mho954 options install 'MHO900-BWU05T08@…'` installiert eine **gekaufte** Lizenz. Die Lizenz
  wird auf rigol.com → License Activation aus dem Key und der Seriennummer erzeugt.
- Das Umgehen der Lizenzprüfung (Keygen, `vendor.bin`-Patch, gepatchte Firmware) ist bewusst
  **nicht** enthalten.

## Bekannte Eigenheiten

Details in `docs/scpi-mho900.md`.

- Das Scope **beantwortet unbekannte Queries nicht**. Danach hängt die Kommunikation, bis ein
  USBTMC-Clear kommt. Der Transport macht das automatisch, der Treiber prüft Parameter vorher.
- Unter USB/IP blockieren die `ABORT_BULK_*`-ioctls im Kernel. Deshalb verwendet der Transport
  nur `CLEAR`.
- `mho954 status` braucht ca. 50 Queries (≈1,3 s). Die GUI lädt ihn deshalb nur alle 5 s und
  nach Änderungen neu.

## Entwicklung

```bash
cargo test --workspace            # inkl. End-to-End-Tests gegen den Simulator
mho954 -c sim serve               # API ohne Hardware
```
