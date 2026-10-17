# MHO900 SCPI – verifizierte Notizen

Quelle: *MHO900 Programming Guide* (Rigol, Feb. 2026, 518 S.) plus Messungen am eigenen
MHO954 (Serie `MHO9X000000000`, `*IDN?`-Firmware `00.01.00`, USB über USB/IP vom Windows-Host).
„Gerät“ = am Scope beobachtet, „Guide“ = laut Programmierhandbuch.

## Transport

| Thema | Befund | Quelle |
|---|---|---|
| USB | `1ab1:0452`, Kernel-Treiber `usbtmc`, `/dev/usbtmc0` bzw. udev-Symlink `/dev/usbtmc_MHO954` | Gerät |
| Latenz | 24–37 ms pro Query, `:MEAS:ITEM?` ≈ 130 ms | Gerät |
| Unbekannte Query | **keine Antwort**; danach blockiert der Message-Austausch, bis `USBTMC_IOCTL_CLEAR` gesendet wird | Gerät |
| Recovery | `CLEAR_OUT_HALT` + `CLEAR_IN_HALT` + `CLEAR` stellt den Betrieb wieder her | Gerät |
| `ABORT_BULK_IN` | hängt unter USB/IP (`vhci_hcd`) im Kernel (`usb_kill_urb`, Prozess in D-State) → **nie verwenden** | Gerät |
| `*OPC?` | liefert `1` (anders als bei DHO800/900, wo es immer `0` liefert) | Gerät |
| `:SYST:VERS?` | `3.0` (SCPI-Version, nicht die Firmware) | Gerät, Guide |
| LAN | `:LAN:STAT?` → `UNLINK`, `:LAN:IPAD?` → `0.0.0.0` (kein Kabel) | Gerät |

## Wellenformen

| Thema | Befund | Quelle |
|---|---|---|
| `:WAV:PRE?` | `1,0,1000,1,2.000000E-8,-1.000000E-5,0.000000,6.6667E-06,0,32768` bei 50 mV/div, 2 µs/div | Gerät |
| Skalierung WORD | `yref = 32768`, `yinc = (V/div)/7500` → 7500 Codes/Division, 16-bit-Vollausschlag ≈ ±4,37 div | Gerät |
| Formel | `V = (raw − yorigin − yreference) × yincrement`, `t = (i − xreference) × xincrement + xorigin` | Guide |
| NORMal | 1000 Punkte, `XINC = TimeScale/100` | Guide |
| RAW | nur im STOP-Zustand; Bereich über `:WAV:STAR`/`:WAV:STOP`, jeder Block mit eigenem `#N`-Header | Guide |
| Default-Format | `BYTE` (Treiber setzt immer `WORD`) | Guide |
| `:WAV:DATA?` über USB/IP | **erster Versuch hing**; die Ursache ist noch offen (siehe unten) | Gerät |

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

## Firmware

Es gibt keinen SCPI-Befehl zum Firmware-Update (Guide durchsucht). Das Update läuft über eine
`.GEL`-Datei auf einem FAT32-Stick am Gerät (`mho954 fw update` zeigt den Ablauf).
`*IDN?` meldet die Version gekürzt (`00.01.00`); die vollständige Version steht unter
Utility → About.

## Offene Punkte (nach Wiederanbindung des Geräts prüfen)

- `:WAV:DATA?` im NORM/WORD-Modus über USB/IP: Beim ersten Test kam keine Antwort. Danach war
  die USB/IP-Verbindung gestört, bis die Windows-Seite sie zurücksetzte. Mit `mho954 probe`
  erneut testen. Die Leseanforderungen sind jetzt auf 256 KiB begrenzt, und `ABORT_*` wird nicht
  mehr verwendet.
- Durchsatz bei RAW-Transfers über USB/IP (`mho954 bench` im STOP-Zustand).
- Ob die 12 Bit im WORD links- oder rechtsbündig liegen (`mho954 probe` meldet „low nibble used“).
