# MHO900 SCPI – verifizierte Notizen

Quelle: *MHO900 Programming Guide* (Rigol, Feb. 2026, 518 S.) plus Messungen am eigenen
MHO954 (Serie `MHO9X000000000`, `*IDN?`-Firmware `00.01.00`, USB über USB/IP vom Windows-Host).
„Gerät“ = am Scope beobachtet, „Guide“ = laut Programmierhandbuch.

## Transport

Standard ist eine eigene USBTMC-Implementierung im Userspace (`crates/mho-scpi/src/usbtmc_user.rs`,
nusb). Der Kernel-Treiber `usbtmc` (`-c usbtmc`) bleibt als Alternative.

| Thema | Befund | Quelle |
|---|---|---|
| USB | `1ab1:0452`; Kernel-Treiber `usbtmc` legt `/dev/usbtmc0` an (udev-Symlink `/dev/usbtmc_MHO954`). Der Userspace-Transport koppelt ihn ab und danach wieder an | Gerät |
| Latenz | 24–37 ms pro Query, `:MEAS:ITEM?` 25–200 ms | Gerät |
| Unbekannte Query | **keine Antwort**; danach antwortet das Scope auf nichts mehr, bis ein USBTMC Device Clear (INITIATE_CLEAR + CHECK_CLEAR_STATUS) kommt. Einzelne Fälle erholen sich damit zuverlässig. **Nach rund fünf unbekannten Queries seit dem Einschalten hängt die Firmware dauerhaft**, auch wenn dazwischen gültige Queries und Clears liegen (Neustart nötig). Neue Befehle höchstens 3 pro Einschaltzyklus ungeprüft testen | Gerät |
| USBTMC-Padding | Das MHO954 sendet **keine** 4-Byte-Füllbytes nach dem Nutzdatenteil | Gerät |
| Große Antworten | kommen in mehreren Transfers (EOM nur im letzten). Eine offene Nachricht wird vor dem nächsten Befehl bzw. vor einem Clear zu Ende gelesen | Gerät |
| Clear mitten in einer Antwort | blockierte die USB-Schnittstelle des Scopes bis zum Neustart → wird nie gemacht | Gerät |
| Abgebrochener Prozess | hinterlässt eine Antwort mit alter bTag in der Leitung. Der Userspace-Transport überspringt sie (`kill -9` während Screenshot/RAW: 5 von 5 erholt) | Gerät |
| Kernel-Treiber `usbtmc` | Transfers > 4 KiB laufen über `usbtmc_generic_read`, das über USB/IP zu früh endet (Rest = „wrong MsgID“); mit `-c usbtmc` sind Reads daher auf 4080 Bytes begrenzt. `ABORT_BULK_IN` hängt im Kernel (D-State) → nie verwenden. Nach Desync („wrong bTag“) hilft nur ein Neustart | Gerät, Kernel-Quelltext |
| `*OPC?` | liefert `1` (anders als bei DHO800/900, wo es immer `0` liefert) | Gerät |
| `:SYST:VERS?` | `3.0` (SCPI-Version, nicht die Firmware) | Gerät, Guide |
| LAN | `:LAN:STAT?` → `UNLINK`, `:LAN:IPAD?` → `0.0.0.0` (kein Kabel) | Gerät |
| TCP | kein Device Clear im Raw-Socket-Protokoll → der Transport baut die Verbindung neu auf | Implementierung |

## Messwerte am Gerät (USB/IP, Userspace-USBTMC, 64-KiB-Requests)

| Vorgang | Zeit |
|---|---|
| `*IDN?` | 24,5 ms (22,8–25,8) |
| Bildschirm-Wellenform 1 Kanal, Preamble gecacht | 28–32 ms → ≈ 30 fps |
| `:MEAS:ITEM? VPP` | 25–200 ms |
| Screenshot PNG 1024×600 (≈ 85–97 kB) | 240–390 ms (durch die PNG-Erzeugung im Scope begrenzt) |
| RAW 10k Punkte | 0,2 s |
| RAW 1M Punkte WORD (2 MB) | 1,51 s ≈ 1,3 MB/s |
| Live-Ansicht (GUI) | 12 fps bei 2 Kanälen, 5,5 fps bei 4 Kanälen |
| `/dev/usbtmc0` nach Prozessende wieder da | 9–16 ms |

## Wellenformen

| Thema | Befund | Quelle |
|---|---|---|
| `:WAV:PRE?` | `1,0,1000,1,2.000000E-8,-1.000000E-5,0.000000,6.6667E-06,0,32768` bei 50 mV/div, 2 µs/div | Gerät |
| Skalierung WORD | `yref = 32768`, `yinc = (V/div)/7500` → 7500 Codes/Division, 16-bit-Vollausschlag ≈ ±4,37 div | Gerät |
| Formel | `V = (raw − yorigin − yreference) × yincrement`, `t = (i − xreference) × xincrement + xorigin` | Guide |
| Kanäle ohne Daten | Ein eingeschalteter Kanal, für den das Scope nichts erfasst (am Testgerät CH1/CH4), liefert durchgehend `0xFFFF`, und das Scope zeichnet ihn nicht; Messungen ergeben `9.9E37` | Gerät |
| NORMal | 1000 Punkte, `XINC = TimeScale/100` | Guide |
| RAW | nur im STOP-Zustand; Bereich über `:WAV:STAR`/`:WAV:STOP`, jeder Block mit eigenem `#N`-Header | Guide |
| Default-Format | `BYTE` (Treiber setzt immer `WORD`) | Guide |
| `:WAV:DATA?` über USB/IP | funktioniert mit Leseanforderungen von 1–64 KiB (1000 Pkt. WORD in ~100 ms); eine 4-MiB-Anforderung blieb unbeantwortet | Gerät |
| Große Antworten | Transfers > 4 KiB holt der Kernel-Treiber mit `usbtmc_generic_read`; über USB/IP endet das zu früh (24553 von 84109 Bytes), der Rest landet als „wrong MsgID“ im nächsten Read → Scope hängt bis zum Neustart. **Lösung:** jede Leseanforderung ≤ 4080 Bytes, EOM per `USBTMC_IOCTL_MSG_IN_ATTR` | Gerät, Kernel-Quelltext |
| Clear während Transfer | `CLEAR` mitten in einer laufenden Blockantwort hat die USB-Schnittstelle des Scopes blockiert (keine Antwort mehr, auch nach erneutem Clear) | Gerät |

## Kanäle / Erfassung / Trigger

| Befehl | Wertebereich | Quelle |
|---|---|---|
| `:CHANn:BWL` | `OFF`, `ON`, `20M`, `250M` | Guide |
| `:CHANn:IMP` | `OMEG`, `FIFTy` (Query: `OMEG`/`FIFT`) | Guide, Gerät |
| `:CHANn:PROB?` | `1.000000E0` | Gerät |
| `:ACQ:TYPE` | `NORMal`, `PEAK`, `AVERages`, `HRESolution` | Guide |
| `:ACQ:MDEP` | `AUTO`, `1k` … `100M` (bis `500M` mit RLU-05); Query z. B. `1.0000E+04`. Maximum je aktive Kanäle: 100M/50M/25M bei 1/2/3–4 Kanälen, mit RLU05 500M/250M/125M (Guide 3.3.2; wird vor dem Senden geprüft) | Guide, Gerät |
| `:TIM:MODE` | `MAIN`, `XY`, `ROLL` | Guide |
| `:TRIG:STAT?` | `TD`, `WAIT`, `RUN`, `AUTO`, `STOP` | Guide, Gerät |
| `:TRIG:COUP` | `AC`, `DC`, `LFReject`, `HFReject` | Guide |
| `:MEAS:ITEM?` | `<item>,<src>[,<src2>]`; ungültig → `9.9E37`. Die erste Abfrage eines neu angelegten Items liefert oft `9.9E37`, erst die nächste den Wert | Guide, Gerät |

Messgrößen (Guide): VMAX VMIN VPP VTOP VBASe VAMP VAVG VRMS OVERshoot PREShoot MARea MPARea
PERiod FREQuency RTIMe FTIMe PWIDth NWIDth PDUTy NDUTy TVMAX TVMIN PSLewrate NSLewrate VUPPer
VMID VLOWer PVRMs PPULses NPULses PEDGes NEDGes RRDelay RFDelay FRDelay FFDelay RRPHase RFPHase
FRPHase FFPHase ACRMs.

## Optionen

| Befehl | Hinweis | Quelle |
|---|---|---|
| `:SYST:OPT:STAT? <type>` | `<type>` ∈ `BND AFG100 AFG50 AUDio CAN-FD FLEX AERO RLU-05 BWU03T05 BWU03T08 BWU05T08`; Antwort `0`/`1` | Guide |
| `:SYST:OPT:STAT?` ohne Parameter | keine Antwort → Timeout | Gerät |
| `:SYST:OPT:VAL? BWU05T08` | `0` (Alias, „backwards compatibility“) | Gerät, Guide |
| `:SYST:OPT:INST <license>` | Format `MHO900-<OPT>@<code>`; Lizenz auf rigol.com → License Activation aus gekauftem Key + Seriennummer | Guide |
| `:SYST:OPT:UNIN` | entfernt alle offiziellen Optionen, danach Neustart | Guide |
| `*OPT?`, `:SYST:OPT?` | existieren nicht (keine Antwort) | Gerät |

## Upgrade-Optionen und Fähigkeiten (`mho954 caps`)

Stand 2026-10-18, nach dem Upgrade des Test-Scopes:

| Befund | Quelle |
|---|---|
| Aktiv: BWU05T08, RLU05, AFG50, AFG100, AUTOA (CAN-FD), FlexA, AUDIOA (I²S), AEROA (1553) | Gerät (`:SYST:OPT:STAT?`, über LAN) |
| `*IDN?` meldet nach BWU05T08 weiter `MHO954` (Firmware `00.01.00`); die effektive Bandbreite (800 MHz bei 1–2 Kanälen, 400 MHz bei 3–4) ergibt sich nur aus den Optionen | Gerät, Guide-Modelltabelle |
| `:SYST:MOD?` bleibt `1,0,0,0,0`, auch nach Neustart mit aktivem AFG100: das DG-Feld spiegelt die Lizenz nicht wider. Der Generator gilt als vorhanden, wenn DG = 1 **oder** eine AFG-Lizenz aktiv ist **und** `:SOUR1:OUTP:STAT?` am Gerät beantwortet wurde (`mho954 verify afg`) | Gerät |
| `:SYST:DGST?` (DG vorhanden) | Guide 3.24; noch ungeprüft (Probe `system.dg`) |
| EMBD, COM, AUTO sind Lizenznamen anderer Serien und beim MHO900 Standard; sie haben kein Status-Token und werden nie abgefragt | Guide 3.24.18 |
| Kein SCPI-Befehl für die Bandbreite (`:SYST:BAND?` o. ä.) | Guide durchsucht |

Gruppen mit Option prüfen die Lizenz vor jedem Lesen und Schreiben (`requires: {"option": "FlexA"}`).

## Funktionsgenerator (AFG, Guide 3.25)

| Befehl | Hinweis | Quelle |
|---|---|---|
| `:SOUR<n>:FUNC` | `SINusoid SQUare RAMP NOISe DC ARB EXPRise EXPFall ECG1 GAUSsian LORentz HAVersine SINC` | Guide |
| `:SOUR<n>:FREQ` | Sinus 2 mHz–100 MHz (AFG100) / 20 mHz–50 MHz (AFG50); Rechteck, Arb und eingebaute Formen 20/10 MHz; Rampe 2/1 MHz; DC und Rauschen ohne Frequenz | Guide |
| `:SOUR<n>:VOLT:AMPL` | 50 Ω: 1 mVpp–10 Vpp (≤ 50 MHz), 5 Vpp darüber; High-Z: doppelt so viel | Guide |
| `:SOUR<n>:VOLT:OFFS` | ±(Vmax − Vpp)/2 | Guide |
| `:SOUR<n>:PER`, `:VOLT:HIGH`, `:VOLT:LOW` | gekoppelt an Frequenz bzw. Amplitude/Offset | Guide |
| `:SOUR<n>:IMP` | `OMEG` (High-Z) / `FIFTy` (50 Ω): erwartete Last, bestimmt die angezeigte Amplitude | Guide |
| `:SOUR<n>:PHAS:SYNC` | „Align phase“ für GI und GII (Befehl `afg.align_phase`) | Guide |
| `:SOUR<n>:LOAD:ARB <C:/…\|D:/…>` | lädt eine Arb-Datei vom Scope/USB-Stick; ein SCPI-Upload existiert nicht (Befehle `afg1.load_arb`, `afg2.load_arb`, Ablesewert `afg.arb`) | Guide |
| `:SOUR<n>:MOD:*` | AM/FM/PM mit internem Modulator (Gruppe `afg.modulation`) | Guide |
| Bündel-, Sweep-, Burst-, Kopplungsbefehle | gibt es nicht | Guide |

Die Grenzen werden vor dem Schreiben geprüft (`crates/mho-driver/src/generator.rs`). Der Ausgang
wird zuletzt eingeschaltet, beim Ausschalten zuerst.

## Bode-Diagramm

- **Am Scope** (`:BODeplot`, Guide 3.5): Start 10 Hz–3 MHz, Stop 100 Hz–30 MHz, Start ≤ Stop/10,
  10–100 Punkte/Dekade, Amplitude 20 mV–10 V gesamt (`ALL`) oder je Bereich (`10 100 1K 10K 100K
  1M 10M 25M`). Kurve nur auf dem Bildschirm, **kein Export-Befehl** → `bode --native` liefert
  einen Screenshot. Knoten `:BOD:PHASE:ENAB` (Langform `PHASEcurve`); `:BOD:PHAS:ENAB` aus der
  ersten Tabellenfassung wäre unbekannt gewesen.
- **Software-Sweep** (`bode`): Generator-Schritte, eine Single-Erfassung je Punkt, Ein-Bin-DFT
  auf beiden Kanälen derselben Erfassung (Phase daher unabhängig vom Trigger), optional
  komplex gemittelt. Auswertung: −3 dB, 0-dB-Durchgang, Phasen-/Amplitudenreserve,
  entfaltete Phase (`mho_dsp::bode_summary`).

## Optionale Protokolle (Guide 3.4.14–18, 3.27.25–27)

| Gruppe | Befehle | Option |
|---|---|---|
| `bus.canfd` | `:BUS<n>:CAN:FDBaud` 1–10 Mbit/s, `:FDSPoint` 10–90 % | AUTOA |
| `bus.flexray` | `:BUS<n>:FLEXray:BAUD` 2,5/5/10 Mbit/s, `SOURce`, `SPOint`, `STYPe BP\|BM\|RT`, `CHANnel A\|B`; Schwelle `:BUS<n>:THR <v>,FLEX` | FlexA |
| `bus.i2s` | `:BUS<n>:IIS:SOURce:CLOCk/DATA/WSELect`, `ALIGnment IIS\|RJ\|LJ`, `CLOCk:SLOPe`, `RWIDth`, `RECewidth`, `WSLow`, `ENDian`, `POLarity`; Schwellen `I2SClk`, `WS`, `DATA` | AUDIOA |
| `bus.m1553` | `:BUS<n>:M1553:SOURce`; Schwelle `1553` | AEROA |
| `trigger.flexray` | `:TRIG:FLEXray:*` (BAUD, POS, ERRor, SYMBol, FRAMe, DEFine, IDCmp, CYCComp, MIN/MAXid, MIN/MAXCy, CH, SOURce, WHEN, LEVel) | FlexA |
| `trigger.i2s` | `:TRIG:IIS:*` (ALIGnment, C/S/DLEVel, WIDTh, UWIDth, CLOCk:SLOPe, Quellen, WHEN, AUDio, DATA) | AUDIOA |
| `trigger.m1553` | `:TRIG:M1553:*` (SOURce, WHEN, POLarity, A/BLEVel, WINDow, SYNC, ERRor, DATComp, DATValue, DRTA) | AEROA |

- `:BUS<n>:MODE` kennt zusätzlich `IIS`, `FLEXray`, `M1553` (Query `IIS`, `FLEX`, `M1553`).
- Für den **CAN-FD-Trigger** dokumentiert der Guide keine Befehle; `:TRIG:CAN:DWIDth` bleibt 1–8
  Byte. FD-Triggerbedingungen lassen sich nur am Gerät einstellen.
- Bitweise Muster (`CODE`, `CURRbit`, `DMIN/DMAX`) der Trigger sind nicht in den Tabellen.
- Die Ereignistabellen (`:BUS<n>:DATA?`) der neuen Protokolle sind am Gerät noch nicht gesehen;
  der Parser ist generisch.

## Netzwerk

| Befehl | Hinweis | Quelle |
|---|---|---|
| `:LAN:DHCP`, `:LAN:AUToip`, `:LAN:MANual` | Bool; nie alle drei aus; Priorität DHCP > Auto-IP > statisch | Guide |
| `:LAN:IPADdress`, `:LAN:SMASk`, `:LAN:GATeway`, `:LAN:DNS` | nur bei statischer IP (DHCP und Auto-IP aus); erstes Oktett 0–223 außer 127 | Guide |
| `:LAN:MAC?`, `:LAN:DSERver?`, `:LAN:VISA? [USB\|LXI\|SOCKet]` | nur Abfrage | Guide |
| `:LAN:STATus?` | `UNLINK`, `CONNECTED`, `INIT`, `IPCONFLICT`, `BUSY`, `CONFIGURED`, `DHCPFAILED`, `INVALIDIP`, `IPLOSE` | Guide, Gerät (`UNLINK`) |
| `:LAN:MDNS`, `:LAN:HOST:NAME`, `:LAN:DESCription` | mDNS standardmäßig aus | Guide |
| `:LAN:APPLy` | übernimmt alle `:LAN`-Änderungen | Guide |
| WLAN | **keine SCPI-Befehle** im Guide (Feb. 2026). Am Gerät geprüft und unbeantwortet: `:WLAN:SSID?`, `:WLAN:STATus?`, `:WLAN:IPADdress?`, `:WIFI:STATus?`, `:SYSTem:WLAN:STATus?`, `:SYSTem:WIFI:STATus?`, `:LAN:WLAN:STATus?`, `:LAN:WIFI:SSID?` | Guide, Gerät |

## Firmware

Es gibt keinen SCPI-Befehl zum Firmware-Update (Guide durchsucht). Das Update läuft über eine
`.GEL`-Datei auf einem FAT32-Stick am Gerät (`mho954 fw update` zeigt den Ablauf).
`*IDN?` meldet die Version gekürzt (`00.01.00`); die vollständige Version steht unter
Utility → About.

## Verifikation am Gerät (`mho954 verify`, Ergebnisse in `docs/verified-commands.json`)

| Befehl | Ergebnis |
|---|---|
| `:SYST:OPT:STAT? <token>` für alle 11 Tokens (BWU05T08 … BND) | beantwortet, alle `0` (keine Option installiert) |
| `:SYST:MOD?` | `1,0,0,0,0` → Logikanalysator-Hardware vorhanden, Funktionsgenerator (DG) nicht |
| `*ESR?`, `*STB?`, `*CLS` | beantwortet bzw. angenommen |
| Compound mit parametrierten Abfragen `:MEAS:STAT:ITEM? CURR,…;:MEAS:STAT:ITEM? AVER,…` | **falsch**: Jede Position liefert den Wert der letzten Teilabfrage → Statistik einzeln abfragen |
| `:MEAS:STAT:ITEM? <CURR\|AVER\|MIN\|MAX\|DEV\|CNT>,<item>,<src>` einzeln | korrekt (z. B. VRMS CH3: 1,21 mV / 1,23 mV / 1,05 mV / 1,35 mV / 39,5 µV / 1000) |
| `:DVM:CURR?` bei ausgeschaltetem DVM | **keine Antwort** (kostet Budget) → wird nur bei eingeschaltetem DVM abgefragt |
| `:DVM:CURR?` bei eingeschaltetem DVM (ACRM, CH2) | beantwortet (`7.910000E-4`) |
| Compound `:TIM:SCAL?;:CHAN1:SCAL?` | eine Zeile, Werte mit `;` getrennt. Das Scope braucht aber ~20 ms je Teilabfrage (10 Abfragen: 0,2 s statt 0,25 s); der Gewinn ist also klein |
| `:WAV:SOUR D0` | **abgelehnt** (`-200 Command execute failed`) → Digitalkanäle nicht über `:WAV:DATA?` lesbar |
| `:MEAS:ITEM VPP,CHAN1` (Set-Form) | angenommen (Messung anlegen ohne Abfrage) |
| `:BUS1:DATA?` mit Dekoder und Ereignistabelle an (Scope läuft, PARALLEL auf CH1) | `#9…PARALLEL\nTime,Data,\n-10.00us,1,`: Die Blocklänge zählt den Protokollnamen mit, danach folgt nur der Terminator. Direkt nach dem Einschalten kam innerhalb von 1,5 s keine Antwort; `verify` wartet deshalb 8 s. Mit ausgeschaltetem Dekoder lautet die Antwort `null` (4 Byte) |
| `:BUS<n>:PAR:BUS D15D0` / `D7D0` / `D15D8` ohne Logiksonde | **abgelehnt** (`-200`), auch mit `:LA:ENAB ON`. `:SYST:MOD?` meldet nur, dass das Gerät den Logikanalysator unterstützt, nicht, ob eine Sonde steckt. `mho954 logic` meldet das und stellt den Dekoder wieder her |
| Alle Abfragen der Gruppen `bus*`, `math`, `math.fft`, `autoset`, `acquire.extra`, `channel`, `timebase.extra`, `display`, `cursor*`, `save`, `histogram`, `mask`, `search`, `record`, `la`, `la.channel`, `reference` | beantwortet, ohne Einträge in der Fehlerqueue (2026-10-18) |
| `:MATH1:FILT:TYPE?` | **keine Antwort** → aus der Gruppe `math.filter` entfernt (den Filtertyp wählt der Math-Operator LPAS/HPAS/BPAS/BST) |
| `:MATH1:FFT:SEAR:RES?` ohne gefundene Peaks | eine leere Zeile |
| `:HIST:STAT:RES?` | `[Sum:0hits, Peaks:0hits, Max:0, …, Bin width:0, Sigma:0, meanPlusSigma:*****, …]` |
| Ablesewerte (`:CURS:MAN:AXV?` usw.) außerhalb ihres Modus | beantwortet. Die Wächter-Abfrage bleibt trotzdem, weil die Werte dann nichts bedeuten |
| `*TST?` | bewusst nicht getestet (Selbsttest kann länger als der Timeout dauern) |

Bereits durch Hardware-Tests belegt: alle Abfragen von `status`/`channel`/`trigger`/`acquire`/`timebase`
(`:CHANn:UNIT?/VERN?/INV?`, `:TRIG:HOLD?/SWE?`, `:ACQ:AVER?/SRAT?` usw.) und alle 14 `:LAN`-Abfragen.

## Dekoder und Math

- `:BUS<n>:DATA?` liefert `#9…PARALLEL\nTime,Data,\n-2.47us,0,\n…`. Der Treiber liest trotzdem
  bis zum USBTMC-Nachrichtenende (`Scpi::query_block_message`) und parst den ganzen Text.
- `:BUS<n>:THReshold? <Leitung>` (TX, SCL, SDA, CLK, MISO, CAN, LIN, PAL) gehört als „keyed“
  Parameter zur jeweiligen Dekodergruppe. RX/CS/PALCLK fehlen bewusst: Der Guide erlaubt sie nur
  bei aktiver Quelle, und eine unbeantwortete Query kostet Budget.
- `:BUS<n>:PAR:WIDT/BITX/SOUR` gelten nur bei `BUS USER` und fehlen deshalb in der Gruppe.
- `:MATH<n>:FFT:SEARch:RES?` liefert mehrere Zeilen (`1,2.50000MHz,-24.98dBV`). Gelesen wird mit
  `Scpi::query_lines` bis zum Nachrichtenende.
- Math-Kanäle lassen sich nur mit `:WAV:MODE NORM` lesen. FFT-Ergebnisse kommen nicht ins
  Zeit-Live-Bild.
- Druckfehler im Guide: `:BUS <n>:PAR:END` → `:BUS<n>:PAR:END`, `:MATH<n>:MATH<n>:WAVetype`
  (nicht verwendet).

## Offene Punkte

- `verify <gruppe>` nimmt die Untergruppen mit (`bus` → `bus.rs232` …), die Proben `bus.data`
  und `math.peaks` aber nur, wenn sie ausdrücklich genannt werden. Sie verstellen das Scope und
  kosten bei fehlender Antwort Budget. Set-Befehle schickt `verify` immer, auch wenn sie schon
  geprüft sind (Setup und Restore der Proben), und vor jedem Kandidaten wird die Fehlerqueue
  geleert.
- Noch am Gerät zu prüfen: `:MATH<n>:FFT:SEAR:RES?` mit
  echten Peaks (Format der Zeilen), Logikanalysator mit angesteckter Sonde (Bitreihenfolge
  D15 = MSB angenommen).
- Pfade für `:SAVE:*`/`:LOAD:*`: `C:/` oder `D:/`, Dateiname ≤ 26 Zeichen (Guide 3.21). Erlaubt
  sind nur Buchstaben, Ziffern, `_ - . /`, damit kein `;` einen zweiten Befehl einschleusen kann.
- Parameter mit vorangestelltem Index (`:LA:DIG:ENAB D3,ON`, `:REF:SOUR 2,CHAN1`,
  `:BOD:VOLT ALL,0.5`) sind „prefixed“ in der Tabelle; Abfrage jeweils `…? <index>`.
- `afg*`, `bode*` und die Options-Gruppen sind seit dem Upgrade verfügbar, aber noch am Gerät
  zu prüfen (siehe unten, Reihenfolge der Prüfblöcke).
- Nicht umgesetzt: `:NAVigate`, `:SAVe:SMB` (Netzlaufwerk mit Passwort), `*TST?` (Laufzeit
  länger als der Timeout).

### Prüfblöcke nach dem Upgrade (je Block ein Einschaltzyklus, über USB)

`verify` überspringt nach einer unbeantworteten Query den Rest der Gruppe und ihre
Untergruppen; ein fehlender Generator kostet so 1 statt 3 Budget-Einheiten.

| Block | Befehl | Aufbau |
|---|---|---|
| A | `mho954 verify --reset-budget afg system.dg` | GI → CH1, GII → CH2 |
| B | `mho954 verify bode` | GI über T-Stück an CH1 und CH2 (oder RC-Glied) |
| C | `mho954 verify bus.canfd bus.flexray` | – |
| D | `mho954 verify bus.i2s bus.m1553` | – |
| E | `mho954 verify trigger.flexray trigger.i2s trigger.m1553` | – |

Danach `~/.local/state/mho954/verified.json` in `docs/verified-commands.json` übernehmen und
neu bauen; Hardwaretests: `MHO954_HW=1 MHO954_HW_AFG=1 cargo test -p mho-driver --test hw --
--ignored --test-threads=1`.
