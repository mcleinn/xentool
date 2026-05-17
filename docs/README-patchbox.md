# Patchbox OS setup — Exquis + Wooting in parallel

Full setup guide for running xentool on a Raspberry Pi 5 with Patchbox OS,
MODEP, and the pisound HAT. Both the Exquis (MPE controller) and Wooting
(analog keyboard) backends run simultaneously, each with its own Live HUD,
connected through MODEP for audio effects.

## Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│ Patchbox OS (Raspberry Pi 5 + pisound)                              │
│                                                                     │
│  Exquis ×4 (USB) ──► xentool serve (exquis) ──► JACK/MODEP         │
│                          │  Live HUD :9099                          │
│                          │  SuperCollider (tanpura) ──► pisound out │
│                          └─ tanpura_studio :9100                    │
│                                                                     │
│  Wooting (USB) ──► xentool serve (wooting) ──► JACK/MODEP          │
│                          │  Live HUD :9199                          │
│                          └─ (MODEP effects) ──► pisound out         │
│                                                                     │
│  XenHarm service :3199 (optional note name glyphs)                  │
│  MODEP web UI :80                                                   │
└─────────────────────────────────────────────────────────────────────┘
```

## Prerequisites

- Raspberry Pi 5 (4 GB+ RAM)
- [Patchbox OS](https://blokas.io/patchbox-os/) with MODEP module active
- pisound HAT (or other JACK-compatible audio interface)
- USB-connected Exquis controller(s) and/or Wooting analog keyboard

## 1. Install xentool

```bash
# Clone and build
cd /opt
sudo git clone https://github.com/<your-repo>/xentool.git
sudo chown -R patch:patch /opt/xentool
cd /opt/xentool

# Run the installer (handles Rust toolchain, SDK dependencies, etc.)
bash scripts/install-linux-wooting.sh   # for Wooting (includes Analog + RGB SDKs)
bash scripts/install-linux-exquis.sh    # for Exquis
```

The installer builds xentool and places the binary at `~/.cargo/bin/xentool`.

## 2. Layout files

Place your layout files in the repo:

```
/opt/xentool/wtn/edo53.wtn    # Wooting layout (53-EDO example)
/opt/xentool/xtn/edo53.xtn    # Exquis layout (53-EDO example)
```

Create new layouts with `xentool new` or edit existing ones with `xentool edit`.

## 3. XenHarm sidecar (optional)

Provides microtonal note name glyphs to the Live HUD:

```bash
mkdir -p ~/.local/share/xentool
python3 -m venv ~/.local/share/xentool/venv
~/.local/share/xentool/venv/bin/pip install flask xenharmlib

cat > ~/.config/systemd/user/xenharm.service << 'EOF'
[Unit]
Description=XenHarm note name service (xentool sidecar)
After=network.target

[Service]
Type=simple
ExecStart=/home/patch/.local/share/xentool/venv/bin/python /opt/xentool/xenharm_service/server.py --host 127.0.0.1 --port 3199
WorkingDirectory=/opt/xentool/xenharm_service
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now xenharm.service
```

## 4. Exquis backend service

```bash
cat > ~/.config/systemd/user/xentool.service << 'EOF'
[Unit]
Description=xentool serve (exquis backend, Live HUD on http://localhost:9099/)
After=network.target sound.target xenharm.service
Wants=xenharm.service

[Service]
Type=oneshot
RemainAfterExit=yes
# Wait for xenharm HTTP to actually answer (process startup != port ready).
# Without this, the HUD's one-shot health probe fails at boot and glyphs stay
# disabled until xentool is restarted manually.
ExecStartPre=/bin/sh -c 'for i in $(seq 1 60); do curl -sf http://127.0.0.1:3199/health >/dev/null && exit 0; sleep 1; done; exit 0'
ExecStart=/usr/bin/tmux -L xentool new-session -d -s xentool '/home/patch/.cargo/bin/xentool serve /opt/xentool/xtn/edo53.xtn --hud --hud-port 9099'
ExecStop=-/usr/bin/tmux -L xentool kill-session -t xentool

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool.service
```

Attach to TUI: `tmux -L xentool attach`

## 5. Wooting backend service

```bash
cat > ~/.config/systemd/user/xentool-wooting.service << 'EOF'
[Unit]
Description=xentool serve (wooting backend, Live HUD on http://localhost:9199/)
After=network.target sound.target xenharm.service
Wants=xenharm.service

[Service]
Type=oneshot
RemainAfterExit=yes
# Wait for xenharm HTTP to actually answer (process startup != port ready).
ExecStartPre=/bin/sh -c 'for i in $(seq 1 60); do curl -sf http://127.0.0.1:3199/health >/dev/null && exit 0; sleep 1; done; exit 0'
ExecStart=/usr/bin/tmux -L xentool-wooting new-session -d -s xentool-wooting '/home/patch/.cargo/bin/xentool serve /opt/xentool/wtn/edo53.wtn --hud --hud-port 9199 --osc-port 9010'
ExecStop=-/usr/bin/tmux -L xentool-wooting kill-session -t xentool-wooting

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool-wooting.service
```

Attach to TUI: `tmux -L xentool-wooting attach`

## 6. MODEP MIDI integration (Wooting)

### Background

MODEP's web UI only shows MIDI devices that have JACK ports with the
`JackPortIsPhysical` flag. JACK's `-X seq` backend (configured in
`/etc/jackdrc`) bridges ALSA sequencer ports to JACK, but only marks ports as
physical if they have `SND_SEQ_PORT_TYPE_HARDWARE` in their ALSA type flags.

xentool's Wooting backend creates its ALSA port with hardware type flags
specifically for this reason. However, JACK only picks up ALSA ports that exist
at JACK startup time. Since the xentool user service starts after the JACK
system service, we need a helper that restarts JACK (and MODEP) once xentool is
running.

### JACK restart helper

```bash
mkdir -p ~/.local/bin

cat > ~/.local/bin/xentool-wooting-jack-connect << 'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
export JACK_PROMISCUOUS_SERVER=jack

# Wait for xentool-wooting's ALSA sequencer port to appear
for _ in $(seq 1 40); do
  if arecordmidi -l 2>/dev/null | grep -q "xentool-wooting"; then
    break
  fi
  sleep 0.5
done

if ! arecordmidi -l 2>/dev/null | grep -q "xentool-wooting"; then
  echo "xentool-wooting ALSA port not found after 20s, skipping" >&2
  exit 0
fi

# Restart JACK so -X seq picks up the port with physical flag
sudo systemctl restart jack

# Wait for JACK to be ready
for _ in $(seq 1 20); do
  if jack_lsp >/dev/null 2>&1; then break; fi
  sleep 0.5
done

# Reset failed MIDI merger/broadcaster units (they crash when JACK goes down)
# and restart the full MODEP MIDI chain so it discovers the new physical port.
sudo systemctl reset-failed modep-mod-midi-merger modep-mod-midi-broadcaster 2>/dev/null || true
sudo systemctl restart modep-mod-host modep-mod-ui modep-mod-midi-merger modep-mod-midi-broadcaster

# Restart user-level JACK clients (SC + alsa_out) that were killed by the
# JACK restart above. systemctl --user is non-fatal if the units don't exist.
systemctl --user restart xentool-supercollider.service xentool-alsa-out.service 2>/dev/null || true

exit 0
SCRIPT

chmod +x ~/.local/bin/xentool-wooting-jack-connect
```

### Systemd service for the helper

```bash
cat > ~/.config/systemd/user/xentool-wooting-jack-connect.service << 'EOF'
[Unit]
Description=Restart JACK+MODEP after xentool-wooting is up (so MODEP sees the MIDI port)
After=xentool-wooting.service
Wants=xentool-wooting.service

[Service]
Type=oneshot
Environment=JACK_PROMISCUOUS_SERVER=jack
ExecStart=/home/patch/.local/bin/xentool-wooting-jack-connect

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool-wooting-jack-connect.service
```

### Passwordless sudo for service restarts

```bash
sudo tee /etc/sudoers.d/xentool-jack-restart >/dev/null <<'EOF'
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart jack
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart modep-mod-host
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart modep-mod-ui
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart modep-mod-host modep-mod-ui
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart modep-mod-host modep-mod-ui modep-mod-midi-merger modep-mod-midi-broadcaster
patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl reset-failed modep-mod-midi-merger modep-mod-midi-broadcaster
EOF
sudo chmod 440 /etc/sudoers.d/xentool-jack-restart
sudo visudo -c -f /etc/sudoers.d/xentool-jack-restart   # syntax check
```

### Exquis in MODEP

The Exquis controllers are hardware USB MIDI devices — JACK automatically picks
them up as physical ports at boot. No extra steps needed; they appear in MODEP's
MIDI device list immediately.

## 7. SuperCollider synth (optional, Exquis)

For the MPE tanpura synth that runs alongside the Exquis backend:

```bash
cat > ~/.config/systemd/user/xentool-supercollider.service << 'EOF'
[Unit]
Description=SuperCollider synth for xentool (exquis -> mpe_tanpura_xentool.scd)
After=xentool.service sound.target
Wants=xentool.service

[Service]
Type=simple
Environment=QT_QPA_PLATFORM=offscreen
# Wait for JACK to be up. On boot, jack.service (system) usually comes up after
# user services, so sclang would otherwise fail scsynth boot with a name-conflict
# error and never recover (Restart=on-failure doesn't fire because sclang itself
# stays alive even when scsynth fails to boot).
ExecStartPre=/bin/sh -c 'for i in $(seq 1 60); do jack_lsp >/dev/null 2>&1 && exit 0; sleep 1; done; exit 1'
ExecStart=/usr/bin/sclang /opt/xentool/supercollider/mpe_tanpura_xentool.scd
# Reconnect SC -> USB speaker bridge whenever SC restarts. alsa_out's own
# ExecStartPost only runs at alsa_out start, so a standalone SC restart would
# otherwise lose the USB-side connection (the JACK link is process-local).
ExecStartPost=/bin/sh -c 'for i in $(seq 1 30); do jack_lsp 2>/dev/null | grep -q "^USB:playback_1$" && jack_lsp 2>/dev/null | grep -q "^SuperCollider:out_1$" && jack_connect SuperCollider:out_1 USB:playback_1 2>/dev/null; jack_connect SuperCollider:out_2 USB:playback_2 2>/dev/null; jack_lsp -c 2>/dev/null | grep -A1 "^SuperCollider:out_1$" | grep -q "USB:playback_1" && exit 0; sleep 1; done; exit 0'
WorkingDirectory=/opt/xentool/supercollider
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool-supercollider.service
```

## 8. USB speaker output (optional)

If you want audio on a USB speaker (e.g. Zgmicro) in **addition to** pisound's
analog output, route SC's JACK output to a second ALSA device via `alsa_out`.

**Why a bridge?** JACK on Patchbox is configured to `hw:pisound` and can only
own one ALSA card at a time. `alsa_out` is a JACK client that mirrors any
connected signal to a separate ALSA device — so you get pisound + USB
simultaneously.

### 8a. Tell wireplumber to release the USB card

PipeWire's wireplumber auto-claims USB audio cards and holds them even when
suspended, blocking `alsa_out`. Add a per-user rule to skip the device:

```bash
mkdir -p ~/.config/wireplumber/main.lua.d

# Find your speaker's device.name (USB vendor/product string):
pw-cli ls Device | grep -B1 -A4 "alsa_card.usb"

# Then write the rule (adjust the match string for YOUR speaker):
cat > ~/.config/wireplumber/main.lua.d/51-ignore-zgmicro.lua << 'EOF'
-- Tell wireplumber to skip the USB speaker so alsa_out can claim it.
rule = {
  matches = {
    {
      { "device.name", "matches", "alsa_card.usb-0ac8_Zgmicro*" },
    },
  },
  apply_properties = {
    ["device.disabled"] = true,
  },
}
table.insert(alsa_monitor.rules, rule)
EOF

systemctl --user restart wireplumber pipewire-pulse pipewire
```

After this, `pactl list short sinks | grep <speaker>` should return nothing,
and `fuser /dev/snd/controlC<N>` should also be empty.

The rule is harmless when the speaker is unplugged — no card means no match.

### 8b. alsa_out bridge service

```bash
# Find your speaker's card index:
aplay -l | grep -i usb
# Example: card 5: AUDIO [Zgmicro AUDIO]  →  use hw:AUDIO

cat > ~/.config/systemd/user/xentool-alsa-out.service << 'EOF'
[Unit]
Description=JACK -> USB speaker bridge (alsa_out)
After=sound.target
Wants=sound.target

[Service]
Type=simple
# Wait for JACK and the USB card. Without these, alsa_out fails fast on boot.
ExecStartPre=/bin/sh -c 'for i in $(seq 1 60); do jack_lsp >/dev/null 2>&1 && exit 0; sleep 1; done; exit 1'
ExecStartPre=/bin/sh -c 'for i in $(seq 1 30); do [ -e /dev/snd/controlC5 ] && exit 0; sleep 1; done; exit 1'
ExecStart=/usr/bin/alsa_out -j USB -d hw:AUDIO -r 48000 -q 1
# Auto-connect SC's output once both sides exist (idempotent; the SC service
# also does this on its own restart so the chain self-heals).
ExecStartPost=/bin/sh -c 'for i in $(seq 1 20); do jack_connect SuperCollider:out_1 USB:playback_1 2>/dev/null && jack_connect SuperCollider:out_2 USB:playback_2 2>/dev/null && exit 0; sleep 1; done; exit 0'
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now xentool-alsa-out.service
```

If the USB speaker isn't connected at boot, the `[ -e /dev/snd/controlC5 ]`
guard fails after 30 s and the service drops out cleanly — pisound output is
unaffected. Plug the speaker in later and run
`systemctl --user restart xentool-alsa-out` to pick it up.

### 8c. Persist the speaker volume

Set the master volume and store it so `alsa-state.service` restores it on boot:

```bash
amixer -c 5 set PCM 80%
sudo alsactl store 5
```

## 9. Tanpura Studio web UI (optional)

Touch-friendly web interface for tweaking the tanpura synth parameters:

```bash
cat > ~/.config/systemd/user/xentool-studio.service << 'EOF'
[Unit]
Description=tanpura studio web UI for xentool (http://localhost:9100/)
After=xentool-supercollider.service sound.target
Wants=xentool-supercollider.service

[Service]
Type=simple
ExecStart=/home/patch/.local/share/xentool/venv/bin/python /opt/xentool/supercollider/tanpura_studio/server.py
WorkingDirectory=/opt/xentool/supercollider/tanpura_studio
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool-studio.service
```

## Boot sequence

After a full reboot, services start roughly in this order:

1. **JACK** (system) — starts on `hw:pisound` with hardware MIDI ports only
2. **MODEP** (system) — mod-host, mod-ui, mod-midi-merger, mod-midi-broadcaster
3. **xenharm** (user) — note name sidecar (Flask boot takes ~9 s)
4. **xentool** / **xentool-wooting** (user) — Exquis / Wooting serve + Live HUDs
   - Each waits for `curl /health` on xenharm to succeed before launching (else the HUD's one-shot probe sticks at `available=false`).
5. **xentool-supercollider** (user) — tanpura synth
   - Waits for `jack_lsp` to succeed (else scsynth boot races JACK and dies).
   - `ExecStartPost` connects SC → USB bridge (if alsa_out is up).
6. **xentool-alsa-out** (user, optional) — JACK → USB speaker bridge
   - Waits for `jack_lsp` and `/dev/snd/controlC<N>` of the USB device. Drops out cleanly if the speaker isn't plugged in.
7. **xentool-studio** (user) — tanpura web UI :9100
8. **xentool-wooting-jack-connect** (user, oneshot) — once the wooting ALSA port exists, restarts JACK so `-X seq` picks it up as physical, then restarts the full MODEP MIDI chain and the user-side JACK clients (SC + alsa_out) so all connections re-establish.

The JACK restart in step 8 causes a ~1 second audio interruption and bounces
SC + alsa_out. The bridge auto-reconnects via the `ExecStartPost` hooks on
both services.

## Web interfaces

| URL | Service |
|-----|---------|
| `http://<pi-ip>/` | MODEP pedalboard editor |
| `http://<pi-ip>:9099/` | Exquis Live HUD |
| `http://<pi-ip>:9199/` | Wooting Live HUD |
| `http://<pi-ip>:9100/` | Tanpura Studio |

## Verifying

```bash
# Check all services are running
systemctl --user status xentool xentool-wooting xenharm

# Verify Wooting port appears as physical in JACK
export JACK_PROMISCUOUS_SERVER=jack
jack_lsp -p | grep -A1 "system:midi" | grep -B1 "physical" | grep wooting
# Expected: system:midi_capture_N and system:midi_playback_N for xentool-wooting

# Check MODEP sees the port
curl -s http://localhost/jack/get_midi_devices | python3 -m json.tool | grep wooting
# Expected: "xentool wooting MIDI 1 (in+out)"

# Attach to TUIs
tmux -L xentool attach            # Exquis TUI
tmux -L xentool-wooting attach    # Wooting TUI
```

## Troubleshooting

**Wooting not visible in MODEP:**
Run the connect script manually: `~/.local/bin/xentool-wooting-jack-connect`

**MIDI data not flowing:**
Check that MODEP has the device enabled (not just listed). In the MOD-UI MIDI
device settings, toggle "xentool wooting MIDI 1" on.

**Port shows as "(in)" instead of "(in+out)":**
Rebuild xentool — the hardware port type flags may be missing. Verify with:
`jack_lsp -p | grep -A1 system:midi_capture_13` — should show `physical`.

**xentool crashes on startup:**
Check if the Wooting Analog SDK is installed:
`ls /usr/local/lib/libwooting_analog_sdk.so`

**No audio output:**
Verify pisound is the JACK device: `cat /etc/jackdrc` should show `-d hw:pisound`.

**No audio after boot (SC running but silent):**
On rare boots scsynth loses its connection to JACK. Restart SC:
`systemctl --user restart xentool-supercollider.service`.
The `ExecStartPre` JACK-wait should normally prevent this.

**HUD shows no microtonal glyphs:**
The HUD probes xenharm once at startup. If xenharm wasn't ready yet, the HUD
sticks at `available: false`. The `ExecStartPre` health-wait on xentool /
xentool-wooting addresses this; if it still happens (e.g. xenharm crashed and
came back), restart xentool: `systemctl --user restart xentool xentool-wooting`.
Verify with: `curl -s http://localhost:9099/api/live/state | grep -o 'xenharm[^,]*'`.

**No sound on USB speaker:**
Check the JACK bridge is wired:
`jack_lsp -c | grep -A2 SuperCollider:out_1` should list both
`system:playback_1` (pisound) and `USB:playback_1` (alsa_out). If `USB:` is
missing, restart alsa_out: `systemctl --user restart xentool-alsa-out`.
If alsa_out fails with "device already in use", wireplumber is still holding
the card — verify the rule in `~/.config/wireplumber/main.lua.d/` matches the
card's `device.name` (check with `pw-cli ls Device`).

**MODEP MIDI plugins (merger / broadcaster) dead after JACK restart:**
`sudo systemctl restart modep-mod-midi-merger modep-mod-midi-broadcaster`.
The wooting-jack-connect helper already does this on boot.
