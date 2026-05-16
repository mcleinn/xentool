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

# Restart MODEP so it discovers the new physical port
sudo systemctl restart modep-mod-host modep-mod-ui
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
echo 'patch ALL=(ALL) NOPASSWD: /usr/bin/systemctl restart jack, /usr/bin/systemctl restart modep-mod-host, /usr/bin/systemctl restart modep-mod-ui, /usr/bin/systemctl restart modep-mod-host modep-mod-ui' \
  | sudo tee /etc/sudoers.d/xentool-jack-restart
sudo chmod 440 /etc/sudoers.d/xentool-jack-restart
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
ExecStart=/usr/bin/sclang /opt/xentool/supercollider/mpe_tanpura_xentool.scd
WorkingDirectory=/opt/xentool/supercollider
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable xentool-supercollider.service
```

## 8. Tanpura Studio web UI (optional)

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

After a full reboot, services start in this order:

1. **JACK** (system) — starts with hardware MIDI ports only
2. **xenharm** (user) — note name sidecar
3. **xentool** (user) — Exquis serve + Live HUD :9099
4. **xentool-wooting** (user) — Wooting serve + Live HUD :9199
5. **xentool-wooting-jack-connect** (user) — waits for Wooting port, restarts JACK + MODEP
6. **xentool-supercollider** (user) — tanpura synth
7. **xentool-studio** (user) — tanpura web UI :9100
8. **MODEP** (system, restarted by step 5) — all MIDI devices visible

The JACK restart in step 5 causes a ~1 second audio interruption. This is
unavoidable since JACK's `-X seq` doesn't hot-plug software ALSA ports.

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
