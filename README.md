# mho954

Steuerung des **Rigol MHO954** (MHO900-Serie) unter Linux, in Rust. Das Programm bietet:

- **Live-Ansicht** (egui): Kanäle in Scope-Farben, Cursor, Trigger-Marker, lokale und
  scope-seitige Messungen, FFT, Screenshot, CSV- und RAW-Export, LAN-Einstellungen
- **CLI** für alle Grundfunktionen, Messungen, Captures, Backup/Restore, LAN und eine SCPI-Konsole
- **REST-API** mit OpenAPI-Dokumentation und einem **WebSocket-Livestream**
- **MCP-Server** für LLM-Agenten (Claude Code u. a.), per HTTP oder stdio
- einen **Simulator**, der die Eigenheiten des echten Geräts nachbildet

Die Verbindung läuft über USB (eigene USBTMC-Implementierung im Userspace, robust auch über
USB/IP) oder LAN/WLAN (Raw-SCPI-Port 5555). NI-VISA wird nicht benötigt.
**Netzwerk ist optional:** Der volle Funktionsumfang läuft über USB.

## Aufbau

```
crates/
  mho-scpi     Transporte: USBTMC im Userspace (nusb), Kernel-usbtmc, TCP:5555; Framing, Recovery
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
sudo contrib/install-udev.sh              # udev-Regel für USB-Zugriff (Gruppe dialout)
```

Der Standard-Transport `usb` braucht Schreibrechte auf `/dev/bus/usb/BBB/DDD` des Scopes. Die
Regel in `udev/62-rigol-usbip.rules` vergibt sie an die Gruppe `dialout`; der Benutzer muss Mitglied
sein. Während eine Verbindung offen ist, wird der Kernel-Treiber `usbtmc` abgekoppelt und danach
wieder angehängt (`/dev/usbtmc0` erscheint nach ~15 ms wieder).

Optional:
- `contrib/mho954.service` – systemd-User-Unit für den Daemon
- `contrib/mho954.desktop` – Startmenü-Eintrag für die GUI

## Schnellstart

```bash
mho954 idn                                 # Gerät identifizieren
mho954 status                              # alle Einstellungen (--json für JSON)
mho954 gui                                 # Live-Ansicht (startet eingebetteten Server)
mho954 gui --sim                           # dasselbe mit Simulator

mho954 channel 1 --on --scale 0.5 --coupling dc --bwl 20M
mho954 timebase --scale 200e-6
mho954 trigger --source CHAN1 --slope rising --level 1.5 --sweep normal
mho954 run | stop | single | auto | force
mho954 measure VPP FREQ RTIM -s 1          # Messung am Scope (--list: alle Items)
mho954 measure --local -s 1                # lokal aus der Bildschirm-Wellenform
mho954 fft -s 1 -n 5
mho954 screenshot -o shot.png
mho954 stop && mho954 capture -s 1 --raw -o cap.npy   # voller Speicher, + cap.json
mho954 scpi ':ACQ:SRAT?'                   # einzelner Befehl (Blöcke werden erkannt)
mho954 scpi                                # interaktive Konsole
mho954 backup                              # IDN, Status, Optionen, LAN, Live-Config, Setup, Screenshot
mho954 restore ~/mho954-backups/<datum>/setup.bin
mho954 net                                 # LAN-Einstellungen (RJ45)
mho954 net set --static --ip 192.168.1.50 --mask 255.255.255.0 --gateway 192.168.1.1
mho954 net set --dhcp --mdns true --hostname mho954
mho954 options                             # Optionen; options install 'MHO900-…@…'
mho954 fw status | fw update               # Firmware-Version / Update-Ablauf
mho954 bench | probe -o report.json        # Latenz/Durchsatz, SCPI-Dialekt (ohne laufenden Daemon)
```

Globale Optionen: `-c/--connection`, `--url`, `--direct`, `--config`, `-v`/`-vv`/`-vvv`.

| Verbindung | Bedeutung |
|---|---|
| `usb` (Standard), `usb:SERIAL` | USBTMC im Userspace; nur MHO900 (PID 0452), sonst per Seriennummer |
| `usbtmc`, `/dev/usbtmcN` | Linux-Kernel-Treiber (über USB/IP weniger robust) |
| `tcp:HOST[:PORT]` | LAN/WLAN, Raw-SCPI auf Port 5555 |
| `sim` | Simulator; `MHO954_SIM=latency=25,nosignal=1+4` stellt Latenz und Kanäle ohne Signal ein |

Umgebungsvariablen: `MHO954_CONFIG`, `MHO954_CONNECTION`, `MHO954_URL`, `MHO954_SIM`,
`MHO954_ALLOW_ANY_MODEL=1` (andere Rigol-Modelle zulassen), `RUST_LOG`.

**WLAN:** Für den WLAN-Dongle gibt es keine SCPI-Befehle. Das WLAN wird am Scope eingerichtet.
Mit der WLAN-IP des Scopes funktioniert danach `-c tcp:<IP>` ganz normal.

## Daemon, REST und WebSocket

```bash
mho954 serve                      # 127.0.0.1:8954
mho954 serve --allow-raw-scpi     # zusätzlich POST /api/v1/scpi
```

Ctrl-C und SIGTERM beenden den Daemon sauber: Eine laufende Übertragung wird zu Ende gelesen,
bevor das Gerät freigegeben wird.

| Methode | Pfad | Zweck |
|---|---|---|
| GET | `/api/v1/info`, `/api/v1/status` | Kennung, kompletter Zustand |
| GET/PUT | `/api/v1/channels/{n}`, `/timebase`, `/trigger`, `/acquire` | lesen/ändern (nur gesetzte Felder) |
| POST | `/api/v1/actions/{run,stop,single,autoscale,clear,force}` | Bedienung |
| GET/POST | `/api/v1/measure?items=VPP,FREQ&source=CHAN1` | Messungen am Scope |
| GET | `/api/v1/measure/items` | Katalog der Messgrößen |
| GET | `/api/v1/measure/local?source=CHAN1` | lokale Messungen (`has_data`) |
| GET | `/api/v1/fft?source=CHAN1&window=flattop&peaks=5&raw=false&full=false` | Spektrum |
| GET | `/api/v1/waveform/{src}?mode=norm\|raw&format=json\|csv\|npy\|bin` | Wellenform (Standard json; npy mit Header `X-Preamble`) |
| GET | `/api/v1/screenshot.png` | Bildschirm |
| GET/POST | `/api/v1/options`, `/api/v1/options/install` | Optionen, gekaufte Lizenz installieren |
| GET/PUT | `/api/v1/setup` | Setup-Blob sichern/laden |
| GET/PUT | `/api/v1/network` | LAN: DHCP/Auto-IP/statisch, IP, Maske, Gateway, DNS, mDNS, Hostname |
| GET/PUT | `/api/v1/live` | Live-Loop: fps, Messliste (max. 10) |
| POST | `/api/v1/scpi` | Raw-SCPI (nur mit `--allow-raw-scpi`) |
| WS | `/api/v1/stream` | Live-Frames (binär) + Messwerte (JSON) |

Fehler kommen als JSON `{kind, message}` mit HTTP-Status: 400 ungültig, 403 verboten, 409 falscher
Zustand (z. B. RAW während Run), 422 Gerätefehler, 502 Protokollfehler, 503 Scope nicht erreichbar,
504 Timeout.

Doku: <http://127.0.0.1:8954/docs>, OpenAPI: `/api/v1/openapi.json`.

**WebSocket-Frames:** `[u32 LE Headerlänge][JSON-Header][u16 LE Samples aller Spuren]`. Der
Header enthält `seq`, `timestamp_ms`, `trigger_status`, `fps` und `traces[]` mit `source`,
`preamble` und `len`. Volt = `(raw − y_origin − y_reference) × y_increment`.
Text-Nachrichten: `{"type":"measurements",...}` oder `{"type":"error",...}`.

**Sicherheit:**
- **Loopback-Bind (Standard):**
  - Anfragen mit fremdem `Host` werden abgewiesen (Schutz vor DNS-Rebinding).
  - Ebenso schreibende Anfragen und WebSocket-Verbindungen mit fremdem `Origin` (Schutz vor Websites im Browser).
- **LAN-Bind** (`bind = "0.0.0.0:8954"`): `api_token` muss gesetzt sein. Ohne Token startet der Server nicht. Alle Clients senden ihn als `Authorization: Bearer <token>`; CLI und GUI tun das aus der Config.

## MCP

```bash
# HTTP (Daemon oder GUI muss laufen):
claude mcp add --transport http mho954 http://127.0.0.1:8954/mcp
# oder stdio (nutzt den Daemon, falls er läuft, sonst direkt das Gerät):
claude mcp add mho954 -- mho954 mcp
```

Die GUI weicht auf einen freien Port aus, wenn 8954 belegt ist. `gui --sim` nutzt immer einen
freien Port, damit die CLI nicht versehentlich den Simulator steuert.

| Tool | Funktion |
|---|---|
| `identify`, `get_status` | Gerätekennung und Zustand |
| `configure_channel`, `configure_timebase`, `configure_trigger`, `configure_acquisition` | Einstellungen ändern |
| `run_control` | run, stop, single, autoscale, clear, force |
| `measure`, `list_measurement_items` | Messungen am Scope |
| `analyze_waveform` | lokale Messungen (`has_data`) |
| `get_waveform` | dezimierte Samples; `save_as` speichert CSV/NPY im Capture-Ordner (nie überschreibend) |
| `fft_peaks` | Spektrum-Peaks |
| `screenshot` | Bild als MCP-Image |
| `list_options` | Optionen |
| `get_network`, `configure_network` | LAN-Einstellungen (Änderungen nur nach Rückfrage) |
| `raw_scpi` | nur mit `allow_raw_scpi` |

## Konfiguration

`~/.config/mho954/config.toml` (anzeigen mit `mho954 config`; unbekannte Schlüssel sind ein Fehler):

```toml
connection = "usb"            # usb[:SERIAL] | usbtmc | /dev/usbtmc0 | tcp:HOST[:5555] | sim
bind = "127.0.0.1:8954"
allow_raw_scpi = false
# api_token = "…"             # Pflicht, wenn bind nicht loopback ist
capture_dir = "/home/philipp/mho954-captures"
backup_dir = "/home/philipp/mho954-backups"
latest_firmware = "00.01.00.00.26"

[live]
enabled = true
max_fps = 30.0                # ≤ 100
stopped_fps = 2.0             # ≤ max_fps
measure_interval_ms = 1000    # ≥ 200
preamble_refresh_ms = 1000    # ≥ 100
status_every = 3              # :TRIG:STAT? jeden n-ten Frame
measurements = [{ item = "VPP", source = "CHAN1" }, { item = "FREQ", source = "CHAN1" }]
```

## Firmware und Optionen

- `mho954 fw status` vergleicht die installierte Version mit `latest_firmware`. Das Scope meldet
  per `*IDN?` nur `00.01.00`; die volle Version steht am Gerät unter Utility → About.
- `mho954 fw update` zeigt den Ablauf: Backup, `.GEL` von rigol.com auf einen FAT32-Stick,
  Update am Gerät. Per SCPI gibt es keinen Update-Befehl.
- `mho954 options` listet die Optionen.
- `mho954 options install 'MHO900-BWU05T08@…'` installiert eine **gekaufte** Lizenz. Die Lizenz
  wird auf rigol.com → License Activation aus dem Key und der Seriennummer erzeugt.
- Das Umgehen der Lizenzprüfung (Keygen, `vendor.bin`-Patch, gepatchte Firmware) ist bewusst
  **nicht** enthalten.

## Bekannte Eigenheiten des MHO954

Details in `docs/scpi-mho900.md`.

- **Unbekannte SCPI-Queries:** Das Scope beantwortet sie nicht und dann gar nichts mehr, bis ein
  USBTMC-Device-Clear kommt; der Transport sendet ihn automatisch. Nach rund fünf unbekannten
  Queries seit dem Einschalten hängt die Firmware dauerhaft, und nur ein Neustart hilft. Der Treiber
  prüft alle Parameter vorher. Raw-SCPI sperrt sich, sobald das Scope nicht mehr antwortet.
- **Erste Messung:** Eine neu angelegte Messung liefert beim ersten Abruf `9.9E37`; der Treiber
  fragt einmal nach.
- **Kanäle ohne Daten:** Sie liefern durchgehend `0xFFFF`. Programm, API und GUI markieren sie
  als „no data“ (`has_data: false`).
- **USB-Framing:** Das Scope sendet keine USBTMC-Füllbytes, und große Antworten kommen in mehreren
  Transfers. Eine offene Antwort wird vor jedem neuen Befehl zu Ende gelesen; ein Device Clear
  mitten in einer Antwort blockierte das Scope früher.
- **Kernel-Treiber:** `ABORT_BULK_*` hängt unter USB/IP im Kernel; das betrifft nur `-c usbtmc`.
- **TCP:** Es gibt kein Device Clear; der Transport baut die Verbindung stattdessen neu auf.
- **Status-Abfrage:** `mho954 status` braucht etwa 40 Queries (≈1 s). Die GUI lädt ihn alle 15 s
  oder wenn sich die angezeigten Kanäle ändern.

## Entwicklung

```bash
cargo test --workspace                                   # inkl. Simulator- und End-to-End-Tests
MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1   # echtes Gerät
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
mho954 -c sim serve                                      # API ohne Hardware
```

Die CI (`.forgejo/workflows/ci.yml`) führt fmt, clippy und Tests auf dem Host-Runner `apps-ws` aus.
