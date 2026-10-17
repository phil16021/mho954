# MHO900 SCPI – verifizierte Notizen

Quelle: *MHO900 Programming Guide* (Rigol, Feb. 2026, 518 S.) plus Messungen am eigenen
MHO954 (Serie `MHO9X000000000`, `*IDN?`-Firmware `00.01.00`, USB über USB/IP vom Windows-Host).
„Gerät“ = am Scope beobachtet, „Guide“ = laut Programmierhandbuch.

## Transport

| Thema | Befund | Quelle |
|---|---|---|
| USB | `1ab1:0452`, Kernel-Treiber `usbtmc`, `/dev/usbtmc0` bzw. udev-Symlink `/dev/usbtmc_MHO954` | Gerät |
| Latenz | 24–37 ms pro Query, `:MEAS:ITEM?` ≈ 130 ms | Gerät |
| Unbekannte Query | **keine Antwort**; danach antwortet das Scope auf nichts mehr, bis ein USBTMC Device Clear (INITIATE_CLEAR) kommt. Einzelne Fälle erholen sich damit zuverlässig; **mehrere unbekannte Queries hintereinander** hängen die Firmware trotz Clear auf (Neustart nötig) | Gerät |
| Recovery | `CLEAR_OUT_HALT` + `CLEAR_IN_HALT` + `CLEAR` stellt den Betrieb wieder her | Gerät |
| `ABORT_BULK_IN` | hängt unter USB/IP (`vhci_hcd`) im Kernel (`usb_kill_urb`, Prozess in D-State) → **nie verwenden** | Gerät |
| `*OPC?` | liefert `1` (anders als bei DHO800/900, wo es immer `0` liefert) | Gerät |
| `:SYST:VERS?` | `3.0` (SCPI-Version, nicht die Firmware) | Gerät, Guide |
| LAN | `:LAN:STAT?` → `UNLINK`, `:LAN:IPAD?` → `0.0.0.0` (kein Kabel) | Gerät |

| Desync „wrong bTag“ | Nach Probe + Bench (alle erfolgreich) bekam der nächste Prozess beim Öffnen die Antwort einer *früheren* Anfrage (`wrong bTag: 172 != 204`, Abstand konstant 32); jede neue Anfrage hing danach eine Antwort hinterher. Ein Read ohne Anfrage → Timeout, danach keine Antwort mehr (Neustart nötig). Ursache unbewiesen; Verdacht: Clear-Halt (`usb_clear_halt`) über USB/IP setzt die Data-Toggles auf dem Windows-Host nicht zurück | Gerät |

## Messwerte am Gerät (USB/IP, 4080-Byte-Reads)

| Vorgang | Zeit |
|---|---|
| `*IDN?` | 24,9 ms (23,4–27,3) |
| Bildschirm-Wellenform 1 Kanal, Preamble gecacht | 28,2 ms → ≈ 35 fps |
| `:MEAS:ITEM? VPP` | 24–130 ms |
| Screenshot PNG 1024×600 (≈ 84 kB) | 285–320 ms, ≈ 0,3 MB/s |

## Wellenformen

| Thema | Befund | Quelle |
|---|---|---|
| `:WAV:PRE?` | `1,0,1000,1,2.000000E-8,-1.000000E-5,0.000000,6.6667E-06,0,32768` bei 50 mV/div, 2 µs/div | Gerät |
| Skalierung WORD | `yref = 32768`, `yinc = (V/div)/7500` → 7500 Codes/Division, 16-bit-Vollausschlag ≈ ±4,37 div | Gerät |
| Formel | `V = (raw − yorigin − yreference) × yincrement`, `t = (i − xreference) × xincrement + xorigin` | Guide |
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
| `:ACQ:MDEP` | `AUTO`, `1k` … `100M` (bis `500M` mit RLU-05); Query z. B. `1.0000E+04` | Guide, Gerät |
| `:TIM:MODE` | `MAIN`, `XY`, `ROLL` | Guide |
| `:TRIG:STAT?` | `TD`, `WAIT`, `RUN`, `AUTO`, `STOP` | Guide, Gerät |
| `:TRIG:COUP` | `AC`, `DC`, `LFReject`, `HFReject` | Guide |
| `:MEAS:ITEM?` | `<item>,<src>[,<src2>]`; ungültig → `9.9E37` | Guide, Gerät |

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

## Offene Punkte (nach Wiederanbindung des Geräts prüfen)

- Durchsatz bei RAW-Transfers über USB/IP (`mho954 bench` im STOP-Zustand).
- 12-Bit-Ausrichtung im WORD: Das untere Nibble wird genutzt (Codes z. B. 39594..40710). Die Daten
  sind also nicht einfach um 4 Bit links verschoben, die Preamble-Skalierung gilt direkt.
- Screenshot und RAW-Abruf mit den 4080-Byte-Anforderungen erneut testen; Durchsatz messen.
