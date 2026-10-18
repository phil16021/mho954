# mho954

Steuerung des **Rigol MHO954** (MHO900-Serie) unter Linux, in Rust. Das Programm bietet:

- **Live-Ansicht** (egui): Kanäle in Scope-Farben, ziehbare Zeit- und Pegel-Cursor, Zoom,
  Rechtsklick-Menü, Messungen (auch zwischen den Cursorn), FFT (auch über einen Ausschnitt),
  Funktionsgenerator, Bode-Diagramm (live), Zähler/Voltmeter, Ablesewerte, Screenshot, CSV- und
  RAW-Export, LAN-Einstellungen
- **CLI** für alle Funktionen, Messungen, Captures, Generator, Bode, Backup/Restore, LAN und eine
  SCPI-Konsole
- **REST-API** mit OpenAPI-Dokumentation und einem **WebSocket-Livestream**
- **MCP-Server** für LLM-Agenten (Claude Code u. a.), per HTTP oder stdio
- einen **Simulator**, der die Eigenheiten des echten Geräts nachbildet

Die Verbindung läuft über USB (eigene USBTMC-Implementierung im Userspace, robust auch über
USB/IP) oder LAN/WLAN (Raw-SCPI-Port 5555). NI-VISA wird nicht benötigt.
**Netzwerk ist optional:** Der volle Funktionsumfang läuft über USB.

## Aufbau

```
crates/
  mho-scpi     Transporte: USBTMC im Userspace (nusb, Standard), Kernel-usbtmc, TCP:5555; Framing, Recovery
  mho-driver   typisierte MHO900-Befehle, Fähigkeiten (Bandbreite, Speicher, Optionen), Einstellungs-
               tabellen, Generator, Bode, Logikanalysator, Verifikation, Simulator
  mho-dsp      Messungen, FFT, Bode-Auswertung, Dekoder-/Peak-Tabellen, Min/Max-Dezimierung,
               CSV/NPY/VCD-Export
  mho-service  Geräte-Thread, Live-Loop und -Ereignisse, ScopeApi (lokal + HTTP-Client)
  mho-server   axum: REST /api/v1, WebSocket /api/v1/stream, MCP /mcp, Doku /docs
  mho-gui      eframe/egui-Live-Ansicht
  mho954       Binary (CLI)
```

Ein Prozess besitzt das Gerät exklusiv: der Daemon (`mho954 serve`) oder die GUI mit
eingebettetem Server. Alle anderen Clients (CLI, MCP, weitere GUIs) gehen über dessen API.
Die CLI erkennt einen laufenden Daemon automatisch, auch wenn er gerade mit einem langen
Vorgang beschäftigt ist.

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
mho954 idn                                 # Gerät identifizieren (+ Fähigkeiten in einer Zeile)
mho954 caps                                # Bandbreite (mit Upgrade), Speichertiefe, Generator, Optionen
mho954 status                              # alle Einstellungen (--json für JSON)
mho954 gui                                 # Live-Ansicht (startet eingebetteten Server)
mho954 gui --sim                           # dasselbe mit Simulator

mho954 channel 1 --on --scale 0.5 --coupling dc --bwl 20M --probe 10
mho954 timebase --scale 200e-6 --offset 0
mho954 trigger --source CHAN1 --slope rising --level 1.5 --sweep normal --coupling dc
mho954 acquire                             # Erfassung anzeigen
mho954 acquire --depth 500M --type average --averages 16   # ändern (Grenzen siehe caps)
mho954 run | stop | single | auto | force | clear
mho954 measure VPP FREQ RTIM -s 1          # Messung am Scope (--list: alle Items)
mho954 measure RRDELAY -s 1 --source2 2    # Messgrößen mit zwei Quellen
mho954 measure --local -s 1                # lokal aus der Bildschirm-Wellenform
mho954 fft -s 1 -n 5 --window hann         # Spektrum lokal (rectangular|hann|blackman|flattop)
mho954 screenshot -o shot.png
mho954 capture -s 1 -o screen.csv          # Bildschirm-Wellenform (.csv | .npy + .json | .json)
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
mho954 config                              # wirksame Konfiguration als TOML

mho954 groups trigger --params             # Einstellungsgruppen (Trigger-Typen, Dekoder, Math, …)
mho954 get trigger.pulse                   # Gruppe lesen (-n für Instanzen, z. B. Dekoder 1..4)
mho954 set trigger.pulse --activate when=less upper=2u   # schreiben; --activate wählt den Typ
mho954 stats VPP -s 1 --reset              # Messstatistik (aktuell/Mittel/Min/Max/σ/Anzahl)
mho954 counter | dvm                       # Frequenzzähler, Voltmeter (vorher `set counter enable=true`)
mho954 set bus.rs232 -n 1 --activate baud=115200 tx=CHAN2 tx_threshold=1.6
mho954 decode 1                            # Dekoder-Ereignistabelle (--csv, -o bus1.csv)
mho954 set math -n 1 --activate operator=mult source1=CHAN1 source2=CHAN2
mho954 set math.fft -n 2 --activate source=CHAN2 peak_search=true peaks=5
mho954 peaks 2                             # Peak-Tabelle der Scope-FFT
mho954 set cursor.manual --activate ax=-100u bx=300u && mho954 read cursor.manual
mho954 read --list                         # Ablesewerte: Cursor, Maske, Suche, Histogramm, …
mho954 read histogram --enable             # Modus bei Bedarf einschalten, dann lesen
mho954 do save.setup C:/bench1.stp         # Datei auf dem Scope (C:/ intern, D:/ USB-Stick)
mho954 do --list                           # Befehle mit Voraussetzungen; `do reset --yes` = *RST
mho954 logic --bus 4 -o capture.vcd        # D0..D15 (Logiktastkopf) als VCD für PulseView

mho954 afg 1 --func sin --freq 1k --ampl 1 --load 50 --on   # Generator GI (AFG-Option)
mho954 afg 2 --func squ --freq 10k --duty 25 --mod am --depth 50 --mod-freq 100 --on
mho954 afg 1 --off                         # Ausgang aus; `afg 1` zeigt den Zustand
mho954 do afg.align_phase                  # GI/GII phasengleich neu starten
mho954 do afg1.load_arb D:/wave.csv        # Arb-Datei vom USB-Stick des Scopes
mho954 bode --start 100 --stop 1M --dut-in 1 --dut-out 2 -o bode.csv   # Software-Sweep (AFG)
mho954 bode --start 100 --stop 1M --averages 4 --load 50 --settle 200  # gemittelt, 50-Ω-Last
mho954 bode --start 100 --stop 80M --follow 2   # ohne T-Stück: GI→CH1 gegen GII→CH2
mho954 bode --native --start 100 --stop 1M -o bode.png                  # Bode des Scopes (Bild)
mho954 bode --native --keep --start 100 --stop 1M                       # Plot bleibt auf dem Display
mho954 set bus.flexray -n 1 --activate source=CHAN1 baud=10000000      # Optionen FlexA/AUDIOA/AEROA
mho954 set trigger.i2s --activate clock=CHAN1 word_select=CHAN2 data_source=CHAN3
mho954 verify --list                       # Befehle einzeln am Gerät prüfen (siehe unten)
```

`mho954 <befehl> --help` beschreibt alle Optionen. Globale Optionen: `-c/--connection`, `--url`,
`--direct`, `--config`, `-v`/`-vv`/`-vvv`.

| Verbindung | Bedeutung |
|---|---|
| `usb` (Standard), `usb:SERIAL` | USBTMC im Userspace; nur MHO900 (PID 0452), sonst per Seriennummer |
| `usbtmc`, `/dev/usbtmcN` | Linux-Kernel-Treiber (über USB/IP weniger robust) |
| `tcp:HOST[:PORT]` | LAN/WLAN, Raw-SCPI auf Port 5555 |
| `sim` | Simulator, siehe `MHO954_SIM` |

Umgebungsvariablen: `MHO954_CONFIG`, `MHO954_CONNECTION`, `MHO954_URL`, `MHO954_SIM`,
`MHO954_ALLOW_ANY_MODEL=1` (andere Rigol-Modelle zulassen), `RUST_LOG`.

`MHO954_SIM` (kommagetrennt): Standard ist ein aufgerüstetes MHO954 (alle Optionen des Test-Scopes,
Generator, Logiksonde, Signal auf allen Kanälen; GI an CH1, GII an CH2, ohne GII-Ausgang hängt
ein RC-Tiefpass mit 10 kHz hinter GI an CH2, solange GI einen Sinus ausgibt).

| Option | Wirkung |
|---|---|
| `latency=25` | Antwortzeit pro Query in ms |
| `nosignal=1+4` | Kanäle ohne Daten (0xFFFF) |
| `fullidn` | volle Firmware-Version im `*IDN?` |
| `strict` | ungeprüfte Queries werden wie am Gerät abgewiesen |
| `noopt` | keine Optionen (Stand vor dem Upgrade) |
| `noafg` | kein Generator |
| `noprobe` | keine Logiksonde (D0..D15 als Dekoderquelle → -200) |
| `hang_after=5` | unbekannte Queries, nach denen die simulierte Firmware hängt |

**WLAN:** Für den WLAN-Dongle gibt es keine SCPI-Befehle. Das WLAN wird am Scope eingerichtet.
Mit der WLAN-IP des Scopes funktioniert danach `-c tcp:<IP>` ganz normal.

## Fähigkeiten und Optionen

`mho954 caps` (REST `/api/v1/capabilities`, MCP `get_capabilities`) fasst zusammen, was das Gerät
kann. Es leitet das aus `*IDN?`, den Optionen und `:SYST:MOD?` ab:

- **Bandbreite:** MHO934 350 MHz, MHO954 500 MHz, MHO984 800 MHz, jeweils höchstens 400 MHz mit
  3–4 Kanälen (MHO934: 350 MHz). Mit BWU05T08 bzw. BWU03T08 sind es 800 MHz bei 1–2 Kanälen, mit
  BWU03T05 (MHO934) 500 MHz. `*IDN?` meldet
  nach einem Bandbreiten-Upgrade weiter das ursprüngliche Modell (hier `MHO954`).
- **Speichertiefe:** höchstens 100M/50M/25M Punkte bei 1/2/3–4 Kanälen, mit RLU05 500M/250M/125M.
  Tiefere Werte lehnt das Programm vorher mit klarer Meldung ab; die GUI bietet sie nicht an.
- **Generator:** vorhanden, wenn `:SYST:MOD?` ihn meldet **oder** eine AFG-Lizenz aktiv ist und
  das Gerät auf `:SOUR1:OUTP:STAT?` geantwortet hat. Das DG-Feld von `:SYST:MOD?` bleibt beim
  aufgerüsteten Test-Scope auf 0, obwohl der Generator arbeitet.
- **Protokoll-Optionen:** CAN-FD (AUTOA), FlexRay (FlexA), I²S (AUDIOA), MIL-STD-1553 (AEROA).
  Ihre Gruppen (`bus.canfd`, `bus.flexray`, `bus.i2s`, `bus.m1553`, `trigger.flexray`,
  `trigger.i2s`, `trigger.m1553`) prüfen die Lizenz vor dem Lesen und Schreiben; die GUI blendet
  sie ohne Lizenz aus. Für den CAN-FD-Trigger gibt es keine SCPI-Befehle. Die I²S-Taktschwelle
  teilt sich den Wert mit der SPI-Taktschwelle (Schlüssel `CLK`).
- EMBD, COM und AUTO sind beim MHO900 Standard und werden als `standard` gelistet, nicht abgefragt.

## Funktionsgenerator

Zwei Kanäle GI/GII (Option AFG50/AFG100). `mho954 afg`, REST `PUT /api/v1/generator/{n}`, MCP
`set_generator`, GUI „Function generator (AFG)“, oder die Gruppen `afg`/`afg.modulation`.

- Kurvenformen SIN, SQU, RAMP, NOIS, DC, ARB, EXPR, EXPF, ECG1, GAUS, LOR, HAV, SINC; Frequenz oder
  Periode, Amplitude, Offset oder High/Low-Pegel, Phase, Tastgrad, Symmetrie, Last (High-Z/50 Ω),
  Modulation AM/FM/PM.
- **Grenzen** (vor dem Senden geprüft): Sinus bis 100 MHz (AFG100) bzw. 50 MHz (AFG50), Rechteck,
  Arb und eingebaute Formen bis 20/10 MHz, Rampe bis 2/1 MHz, ab 2 mHz (AFG50: 20 mHz).
  Amplitude bis 10 Vpp an 50 Ω (über 50 MHz 5 Vpp), an High-Z doppelt so viel; Offset höchstens
  ±(Vmax − Vpp)/2.
- Der Ausgang wird zuletzt eingeschaltet (beim Ausschalten zuerst); die ganze Änderung läuft als
  ein Gerätejob, kein anderer Client kommt dazwischen.
- `do afg.align_phase` startet GI und GII phasengleich neu. `do afg1.load_arb D:/x.csv` lädt eine
  Arb-Datei, die auf dem Scope oder seinem USB-Stick liegt (einen SCPI-Upload gibt es nicht).

## Bode-Diagramm

Zwei Wege, beide brauchen den Generator:

- **Software-Sweep** (`mho954 bode`, REST `POST /api/v1/bode`, MCP `bode_sweep`, GUI): je
  Frequenz Generator auf Sinus, eine Einzelaufnahme, Amplitude und Phase beider Kanäle per
  Einzel-DFT aus derselben Aufnahme (die Phase hängt deshalb nicht vom Trigger ab). Die
  Kanalskalen starten passend zur Generatoramplitude und werden je Punkt nachgeregelt. Optional
  komplex gemittelt (`--averages`), mit Last (`--load`) und Wartezeit (`--settle`). Danach werden
  Zeitbasis, Skalen, Offsets, Trigger und Run-Zustand wiederhergestellt, die Ausgänge abgeschaltet.
  Ergebnis: Gain/Phase je Punkt, CSV mit Zusammenfassung (−3 dB, 0-dB-Durchgang, Phasen- und
  Amplitudenreserve, entfaltete Phase). Jeder Sweep geht live über den WebSocket; die GUI zeichnet
  ihn mit, egal wer ihn gestartet hat.
- **Ohne T-Stück** (`--follow 2`): GII läuft frequenzgleich mit und wird je Schritt mit GI
  phasengleich gestartet; gemessen wird CH2/CH1. So lassen sich die Pfade GI→CH1 und GII→CH2
  vergleichen (am Test-Scope bis 10 MHz innerhalb 0,1 dB, konstante Laufzeitdifferenz 6,5 ns),
  oder ein Bauteil zwischen GI und CH2 mit GII als Referenz an CH1 messen.
- **Bode des Scopes** (`--native`, REST `/api/v1/bode/native`, MCP `bode_native`): Das Scope misst
  selbst (Start 10 Hz–3 MHz, Stop 100 Hz–30 MHz, Start ≤ Stop/10, 10–100 Punkte/Dekade). Über SCPI
  gibt es keine Messwerte, das Ergebnis ist ein Screenshot. Es treibt nur GI und stellt die beiden
  Messkanäle (Standard CH1/CH2) auf AC, 1 mV/div und 20 MHz;
  danach werden die Kanäle wiederhergestellt, mit `--keep` bleibt der Plot auf dem Display.

## RAW-Abruf und tiefer Speicher

`capture --raw` (REST `?mode=raw`, GUI „RAW to .npy“) liest den ganzen Speicher eines gestoppten
Scopes in Blöcken; `.npy` wird ohne Zwischenkopie geschrieben.

- Die Speichertiefe ist eine Obergrenze: Bei maximal 4 GSa/s füllt sich der Speicher nur bei
  langsamer Zeitbasis. Bei 200 µs/div sind es ≈10M Punkte, volle 500M brauchen ≥ 12,5 ms/div.
- Durchsatz am Test-Scope: USB ≈ 1,2–1,4 MB/s, LAN ≈ 0,9 MB/s. 500M Punkte (1 GB) dauern über
  USB ≈ 12 Minuten und belegen etwa 1 GB RAM.
- Die CLI schreibt CSV oder JSON bis 10M Punkte und lehnt größere Datensätze vorher ab; darüber
  `.npy` (ADC-Codes + Präambel in `.json`).

## GUI

`mho954 gui` startet die Live-Ansicht mit eingebettetem Server (oder nutzt einen laufenden
Daemon). Sie merkt sich ihre Ansichtseinstellungen in `~/.config/mho954/gui.json`, weicht auf
einen freien Port aus, wenn 8954 belegt ist, und `gui --sim` nutzt immer einen freien Port, damit
die CLI nicht versehentlich den Simulator steuert.

- **Kopfzeile:** Modell, Bandbreite, Speichertiefe, Generator und Protokoll-Optionen, rote Anzeige
  `GI ON`/`GII ON`, Verbindung, Trigger-Status, Bildrate, Fortschrittsbalken langer Vorgänge.
- **Scope-Plot:**
  - Linksklick setzt Cursor A. Cursor (A/B zeitlich, Y1/Y2 Pegel) lassen sich mit der Maus ziehen.
    Mittelklick löscht alle.
  - Rechts-Ziehen zoomt auf ein Rechteck, Mausrad verschiebt, Strg+Mausrad zoomt, Doppelklick
    setzt die Ansicht zurück. Die Leiste `view` hat reset / fit Y / X± / Y±.
  - **Rechtsklick-Menü:** Cursor hier setzen oder löschen; auf A–B zoomen, hinein-/herauszoomen,
    vertikal anpassen, zurücksetzen; „Measure A-B“ (lokale Messungen nur über den Bereich, als
    Zeilen `CHx [A-B]`); FFT über A–B oder den sichtbaren Bereich (orange schattiert); am Scope:
    Trigger-Level hier, Zeit hier zentrieren, sichtbaren Bereich als Zeitbasis übernehmen (mehr
    Details als Zoomen in die 1000 Bildschirmpunkte), Screenshot.
  - Die Cursor-Anzeige zeigt Werte je Kanal, Δt, 1/Δt und ΔY; in der div-Ansicht rechnet sie ΔY je
    Kanal in Volt um.
  - FFT: logarithmische Frequenzachse, automatische Y-Skalierung, eigenes Rechtsklick-Menü,
    Peak-Tabelle der Scope-FFT.
- **Bedienung rechts:** Run/Stop/Single/Auto/Force/Clear; Kanäle (V/div, Offset, Kopplung,
  Impedanz, Bandbreitenbegrenzung, Tastkopf, Invertierung); Zeitbasis; Trigger mit Typauswahl
  (nur verfügbare Typen); Erfassung (Tiefe nur bis zur Grenze, Typ, Anzahl Mittelungen); Capture
  (Screenshot, CSV, „Stop + RAW to .npy“); Live-Bildrate.
- **Advanced settings:** alle Einstellungsgruppen mit Wertebereichen; Gruppen ohne nötige Option
  oder Hardware werden nicht angeboten.
- **Readouts & commands:** Ablesewerte mit „Enable and read“ (schaltet den Modus ein) und
  „Settings…“; Befehle zeigen ihre Voraussetzung, Dateien standardmäßig auf `C:/`.
- **Function generator (AFG):** GI/GII, Kurvenform, Felder je Form, Last, Modulation, Apply
  (Ausgang unverändert), Output ON/OFF (rot, wenn an), Align phase, Arb-Datei laden, Read.
- **Bode plot (AFG):** Software-Sweep oder Bode des Scopes („keep on scope screen“), Last,
  Mittelung, „other generator in step“ (= `--follow`). Das Bode-Fenster öffnet sich bei jedem Sweep
  von selbst, zeichnet live, zeigt den vorigen Sweep grau, die Zusammenfassung und einen CSV-Knopf.
- **Counter & DVM:** ein-/ausschalten, Quelle, Wert jede Sekunde; „Settings…“ öffnet die Gruppe.
- **Network (RJ45)**, **System** (Setup sichern/laden, Optionen, gekaufte Lizenz installieren).
- Unten: Messtabelle (lokal und am Scope, Statistik), Dekoder-Tabelle mit Beschriftung im Plot,
  Logic (VCD) mit Logikanalysator.

## Daemon, REST und WebSocket

```bash
mho954 serve                      # 127.0.0.1:8954 (--bind ADRESSE für andere)
mho954 serve --allow-raw-scpi     # zusätzlich POST /api/v1/scpi
```

Ctrl-C und SIGTERM beenden den Daemon sauber: Eine laufende Übertragung wird zu Ende gelesen,
bevor das Gerät freigegeben wird.

| Methode | Pfad | Zweck |
|---|---|---|
| GET | `/api/v1/info`, `/api/v1/status` | Kennung, kompletter Zustand |
| GET/PUT | `/api/v1/channels/{n}`, `/timebase`, `/trigger`, `/acquire` | lesen/ändern (nur gesetzte Felder) |
| POST | `/api/v1/actions/{run,stop,single,autoscale,clear,force}` | Bedienung |
| GET/POST | `/api/v1/measure?items=VPP,FREQ&source=CHAN1[&source2=CHAN2]` | Messungen am Scope |
| GET | `/api/v1/measure/items` | Katalog der Messgrößen |
| GET | `/api/v1/measure/local?source=CHAN1` | lokale Messungen (`has_data`) |
| GET | `/api/v1/fft?source=CHAN1&window=flattop&peaks=5&raw=false&full=false` | Spektrum |
| GET | `/api/v1/waveform/{src}?mode=norm\|raw&format=json\|csv\|npy\|bin` | Wellenform (Standard json; npy mit Header `X-Preamble`) |
| GET | `/api/v1/screenshot.png` | Bildschirm |
| GET/POST | `/api/v1/options`, `/api/v1/options/install` | Optionen, gekaufte Lizenz installieren |
| GET/PUT | `/api/v1/setup` | Setup-Blob sichern/laden |
| GET/PUT | `/api/v1/network` | LAN: DHCP/Auto-IP/statisch, IP, Maske, Gateway, DNS, mDNS, Hostname |
| GET/PUT | `/api/v1/live` | Live-Loop: fps, Messliste (max. 10) |
| GET | `/api/v1/groups`, GET `/api/v1/groups/{id}?instance=n`, PUT `…?instance=n&activate=true` | Einstellungsgruppen: Katalog, lesen, schreiben |
| GET | `/api/v1/measure/stats?item=VPP&source=CHAN1` | Messstatistik |
| POST | `/api/v1/measure/actions/{reset_statistics,clear_all,clear_totalizer}` | Statistik/Messungen/Totalizer zurücksetzen |
| GET | `/api/v1/counter`, `/api/v1/dvm` | Zähler, Voltmeter (409, solange aus) |
| GET | `/api/v1/capabilities`, `/api/v1/modules` | Bandbreite, Speichergrenzen, Generator, Protokoll-Optionen; LA/AFG |
| PUT | `/api/v1/generator/{n}` `{function, frequency, amplitude, offset, load, modulation, …, output}` | Generator GI/GII (ein Gerätejob, Ausgang zuletzt) |
| GET | `/api/v1/bus/{n}/data?format=json\|csv` | Dekoder-Ereignistabelle |
| GET | `/api/v1/math/{n}/peaks` | Peak-Tabelle der Scope-FFT |
| GET | `/api/v1/readouts`, `/api/v1/readouts/{id}?enable=true` | Ablesewerte (Cursor, Maske, Suche, Histogramm, Speicherstatus, Arb-Dateien); `enable` schaltet den Modus ein |
| GET/POST | `/api/v1/commands`, `/api/v1/commands/{id}` `{"path","confirm"}` | Speichern/Laden, Resets, Record, Referenzen (`path` = Nummer), Generator; Voraussetzungen im Katalog; `reset` nur mit `confirm` |
| GET | `/api/v1/logic/{bus}?format=json\|vcd` | Digitalkanäle über den Parallel-Dekoder |
| POST | `/api/v1/bode` `{start, stop, points, input, output, amplitude, generator, averages, load, follow, settle_ms}` | Software-Bode-Sweep (AFG-Option) |
| POST | `/api/v1/bode/native` `{start, stop, points_per_decade, input, output, amplitude, linear, timeout_s, keep_on_screen}` | Bode-Diagramm des Scopes, Ergebnis als PNG |
| POST | `/api/v1/scpi` | Raw-SCPI (nur mit `--allow-raw-scpi`) |
| WS | `/api/v1/stream` | Live-Frames (binär) und Text-Nachrichten, siehe unten |

Fehler kommen als JSON `{kind, message}` mit HTTP-Status: 400 ungültig, 403 verboten, 409 falscher
Zustand (z. B. RAW während Run, Modus aus), 422 Gerätefehler, 502 Protokollfehler, 503 Scope nicht
erreichbar, 504 Timeout.

Doku: <http://127.0.0.1:8954/docs> (auch `/`), OpenAPI: `/api/v1/openapi.json`.

**WebSocket-Frames:** `[u32 LE Headerlänge][JSON-Header][u16 LE Samples aller Spuren]`. Der
Header enthält `seq`, `timestamp_ms`, `trigger_status`, `fps` und `traces[]` mit `source`,
`preamble` und `len`. Volt = `(raw − y_origin − y_reference) × y_increment`.
Text-Nachrichten: `{"type":"measurements",...}`, `{"type":"error",...}`,
`{"type":"progress",...}` (lange Vorgänge, `total = 0` am Ende) und `{"type":"bode",…}`
(nach jedem Messpunkt eines Bode-Sweeps, egal wer ihn gestartet hat; neue Verbindungen bekommen
den letzten Sweep).

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

Die Server-Anweisungen bitten das Modell, zuerst `get_status` und `get_capabilities` zu lesen und
vor dem Einschalten eines Generatorausgangs, Autoscale, Reset oder Netzwerkänderungen nachzufragen.

| Tool | Funktion |
|---|---|
| `identify`, `get_status`, `get_capabilities` | Gerätekennung, Zustand, Fähigkeiten (Bandbreite, Speichergrenzen, Generator, Optionen) |
| `configure_channel`, `configure_timebase`, `configure_trigger`, `configure_acquisition` | Einstellungen ändern |
| `run_control` | run, stop, single, autoscale, clear, force |
| `measure`, `list_measurement_items` | Messungen am Scope |
| `analyze_waveform` | lokale Messungen (`has_data`) |
| `get_waveform` | dezimierte Samples; `raw=true` voller Speicher; `save_as` speichert CSV/NPY im Capture-Ordner (nie überschreibend) |
| `fft_peaks` | Spektrum-Peaks (lokal berechnet) |
| `list_settings_groups`, `get_settings`, `configure_settings` | alle Einstellungsgruppen (Trigger-Typen, Dekoder inkl. Optionen, Math, Zähler, DVM, Generator, Bode, …) |
| `measurement_statistics`, `measurement_action` | Messstatistik, Zurücksetzen |
| `read_counter`, `read_dvm` | Frequenzzähler, Voltmeter |
| `decode_bus` | Dekoder-Ereignisse |
| `scope_fft_peaks` | Peak-Tabelle der Scope-FFT |
| `list_readouts`, `read_values` | Ablesewerte mit Verfügbarkeit; lesen, `enable` schaltet den Modus ein |
| `list_scope_commands`, `run_scope_command` | Befehle mit Voraussetzungen; Speichern/Laden/Resets/Generator (`reset` erst nach Rückfrage) |
| `logic_capture` | Digitalkanäle D0..D15 (optional als VCD gespeichert) |
| `set_generator` | Generator GI/GII einstellen, Ausgang ein/aus |
| `bode_sweep` | Frequenzgang mit dem eingebauten Generator (averages, load, follow) |
| `bode_native` | Bode-Diagramm des Scopes als Bild (`keep_on_screen`) |
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
- `mho954 options install 'MHO900-BWU05T08@…'` (GUI: System → Install) installiert eine
  **gekaufte** Lizenz. Die Lizenz wird auf rigol.com → License Activation aus dem Key und der
  Seriennummer erzeugt.
- Das Umgehen der Lizenzprüfung (Keygen, `vendor.bin`-Patch, gepatchte Firmware) ist bewusst
  **nicht** enthalten.

## Einstellungsgruppen und Verifikation

Trigger-Typen, Mess-Setup, Zähler, Voltmeter, die Dekoder `bus.*` (1–4), die Math-Kanäle `math*`
(1–4), Kanal- und Zeitbasis-Extras (Label, Deskew, Zoom, Roll, XY), Autoset, Display, Cursor,
Speichern, Histogramm, Maskentest, Suche, Record, Referenzen, Logikanalysator, Generator, Bode und
die Options-Protokolle beschreibt eine Tabelle in `crates/mho-driver/src/params.rs`. Ablesewerte
und einmalige Befehle stehen in `ops.rs`. Daraus entstehen Validierung, CLI (`groups/get/set`),
REST, MCP, das GUI-Panel „Advanced settings“ und die Prüfliste für `mho954 verify`.

Weil das Scope unbekannte Queries nicht beantwortet und nach etwa fünf davon hängt, gilt:
- **Schreiben** geht immer. Fehler landen in der Fehlerqueue und werden gemeldet.
- **Lesen** einer Gruppe, Dekoder-Tabellen, FFT-Peaks, Math-Spuren im Live-Bild und das DVM gibt
  es erst, wenn die Queries am Gerät bestätigt sind. Stand 2026-10-18 (nach dem Upgrade) sind alle
  Gruppen und Ablesewerte bestätigt, auch Generator, Bode und die Options-Protokolle.
- **Ablesewerte** (Cursor, Maske, Suche, Histogramm) brauchen ihren Modus am Scope;
  `mho954 read histogram --enable` (GUI: „Enable and read“, REST `?enable=true`, MCP
  `enable: true`) schaltet ihn bei Bedarf ein. Ohne das nennt die Meldung die Gruppe.
- **Befehle** mit Voraussetzung prüfen sie vorher: `histogram.save/reset` (Histogramm an und mit
  Daten), `mask.create/reset`, `save.mask` (Maskentest an), `record.*` (Aufnahme an). Dateien:
  `C:/` ist der interne Speicher, `D:/` der USB-Stick im Scope.
- **`mho954 verify [gruppen]`** (nur direkt, ohne laufenden Daemon) sendet jede noch nicht
  bestätigte Query einzeln, danach `*IDN?`; Schreibbefehle laufen immer. Ergebnisse landen in
  `~/.local/state/mho954/verified.json` (`-o` für eine andere Datei) und werden danach in
  `docs/verified-commands.json` übernommen; das wird beim nächsten Build einkompiliert.
  - Budget: Nach drei unbeantworteten Queries hört es auf und bittet um einen Neustart des Scopes;
    danach `mho954 verify --reset-budget`. `--assume-used N` rechnet schon verbrauchte mit.
  - Bleibt eine Query unbeantwortet, bevor in diesem Lauf eine Query derselben Gruppe beantwortet
    wurde (schon bestätigte zählen nicht), werden die übrigen Queries der Gruppe und ihre
    Untergruppen übersprungen (Gruppe fehlt vermutlich ganz). Wurde schon eine beantwortet, zählt
    ein späterer Ausfall als einzelner falscher Befehl.
  - Ohne Gruppenangabe prüft `verify` alles. Mit Gruppen nimmt es deren Untergruppen mit, die
    festen Proben (z. B. `bus.data`, `math.peaks`, `dvm.value`, `system.dg`) aber nur, wenn sie
    genau benannt werden; `bus.data` und `math.peaks` verstellen das Scope dafür kurz.
- Der Simulator lässt ungeprüfte Queries zu, damit sich alles ohne Gerät testen lässt;
  `MHO954_SIM=strict` verhält sich wie das Gerät.

## Logikanalysator

`:WAV:SOUR D0` lehnt das MHO954 ab. `logic` richtet deshalb einen Dekoder als 16-Bit-Parallelbus
ein (D15..D0, ohne Takt, ein Ereignis pro Änderung), liest dessen Ereignistabelle und stellt den
Dekoder danach wieder her. Die Zeitauflösung entspricht der Abtastung des Dekoders. Die
Bitreihenfolge (D15 = MSB bei `D15D0`) ist am Gerät noch zu bestätigen. Schwellen und Kanäle stehen
in den Gruppen `la` und `la.channel`. Ohne angesteckte Logiksonde (PLA2216) lehnt das Scope D15..D0
als Dekoderquelle ab; `logic` meldet das.

## Bekannte Eigenheiten des MHO954

Details in `docs/scpi-mho900.md`.

- **Unbekannte SCPI-Queries:** Das Scope beantwortet sie nicht und dann gar nichts mehr, bis ein
  USBTMC-Device-Clear kommt; der Transport sendet ihn automatisch. Nach rund fünf unbekannten
  Queries seit dem Einschalten hängt die Firmware dauerhaft, und nur ein Neustart hilft. Der Treiber
  prüft alle Parameter vorher. Raw-SCPI sperrt sich, sobald das Scope nicht mehr antwortet.
- **Kurzformen:** Die aus den Großbuchstaben des Guides abgeleitete Kurzform stimmt nicht immer
  (`:BOD:PHASE:ENAB?` bleibt unbeantwortet, `:BODeplot:PHASEcurve:ENABle?` geht). Neue Befehle
  stehen deshalb in der Langform in den Tabellen. `:BUS<n>:THR? I2SClk` ist nur schreibbar.
- **Erste Messung:** Eine neu angelegte Messung liefert beim ersten Abruf `9.9E37`; der Treiber
  fragt einmal nach.
- **Kanäle ohne Daten:** Sie liefern durchgehend `0xFFFF`. Programm, API und GUI markieren sie
  als „no data“ (`has_data: false`).
- **Einzelaufnahme:** Direkt nach `:SING` meldet `:TRIG:STAT?` kurz noch das alte STOP, und bei
  langsamer Zeitbasis meldet die Präambel kurz nach STOP 0 Punkte. Der Bode-Sweep wartet deshalb
  die Aufnahmedauer ab und liest unvollständige Datensätze erneut.
- **RAW-Präambel:** Im RAW-Modus meldet sie das eingestellte Lesefenster, nicht den Datensatz
  (bis 500M, der Guide nennt 50M); der Abruf öffnet das Fenster vorher auf die Speichertiefe.
- **Histogramm:** Beim vertikalen Histogramm meldet Firmware 00.01.00 ein falsches `Sigma`
  (3 mV für einen Sinus mit 1 Vpp); für die Streuung `ACRMS` verwenden.
- **Zähler:** Ausgeschaltet antwortet `:COUN:CURR?` mit 0; das Programm meldet dann „aus“.
- **USB-Framing:** Das Scope sendet keine USBTMC-Füllbytes, und große Antworten kommen in mehreren
  Transfers. Eine offene Antwort wird vor jedem neuen Befehl zu Ende gelesen; ein Device Clear
  mitten in einer Antwort blockierte das Scope früher.
- **Kernel-Treiber:** `ABORT_BULK_*` hängt unter USB/IP im Kernel; das betrifft nur `-c usbtmc`.
- **TCP:** Es gibt kein Device Clear; der Transport baut die Verbindung stattdessen neu auf.
  Unbekannte Queries deshalb nur über USB prüfen.
- **Status-Abfrage:** `mho954 status` braucht etwa 40 Queries (≈1 s). Die GUI lädt ihn alle 15 s
  oder wenn sich die angezeigten Kanäle ändern.

## Entwicklung

```bash
cargo test --workspace                                   # inkl. Simulator- und End-to-End-Tests
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
mho954 -c sim serve                                      # API ohne Hardware
```

Hardwaretests (`crates/mho-driver/tests/hw.rs`, nur verifizierte Befehle, `*IDN?` nach jedem
Schritt):

```bash
MHO954_HW=1 cargo test -p mho-driver --test hw -- --ignored --test-threads=1
```

| Variable | Bedeutung |
|---|---|
| `MHO954_HW=1` | Hardwaretests einschalten |
| `MHO954_HW_CONN=tcp:192.168.1.109` | Verbindung (Standard `usb`) |
| `MHO954_HW_AFG=1` | Generatortest; Verkabelung GI → CH1, GII → CH2 (1 MΩ) |
| `MHO954_HW_BODE=1` | Bode-Test; GI über ein T-Stück an CH1 und CH2 (flacher Frequenzgang) |

Die CI (`.forgejo/workflows/ci.yml`) führt fmt, clippy und Tests auf dem Host-Runner `apps-ws` aus.
