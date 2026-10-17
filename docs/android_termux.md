# xentool on Android / Termux

Status: **experimental, Exquis flow only.** Brought up on a Pixel 7
(Android 14, Termux 2026.10 from Google Play, Termux:API app installed)
with the Exquis plugged straight into USB-C. Wooting is not attempted
(no Analog/RGB SDK for Android).

This document lists everything that differs from the Linux (Pi) and
Windows setups, why, and how to run it. The short version:

```bash
pkg install git
git clone https://github.com/mcleinn/xentool.git ~/dev/xentool
cd ~/dev/xentool
bash scripts/install-termux-exquis.sh    # pkg install …, cargo build --release, jack2pulse
scripts/run-termux-exquis.sh             # xtn/edo53.xtn by default, like the other launchers
```

Scripts (counterparts of the Linux/Windows ones; no services or autostart):

- `scripts/install-termux-exquis.sh` — packages (git rust clang pkg-config
  libusb termux-api supercollider jack2 pulseaudio tmux), checks that
  `termux-usb` reaches the Termux:API app, builds xentool and the audio
  bridge, links `~/.local/bin/xentool` and `~/.local/bin/xentool-exquis`.
- `scripts/run-termux-exquis.sh [layout] [serve args]` — the tmux session.
  `LAYOUT=` env like `run-all-exquis.bat`; `XENTOOL_TUI=1`, `JACK_PERIOD`,
  `PULSE_LATENCY_MS`, `PULSE_SINK` tune it.

The launcher opens a tmux session with four windows (jackd, audio bridge,
sclang, xentool). Accept the Android USB permission dialog the first time
after plugging the Exquis in. Detach with `Ctrl-b d`, re-attach by running
the script again.

## 1. MIDI: no ALSA sequencer, no midir

Termux has no `/dev/snd/seq`, and the `midir` crate has no Android
backend (it fails to compile for `aarch64-linux-android`). Changes:

- `Cargo.toml`: `midir` and `arboard` are only dependencies for
  `cfg(not(target_os = "android"))`; `libc` is added for Android.
- `src/exquis/usbmidi.rs` (Android only) is a USB-MIDI 1.0 class driver on
  top of `rusb`: finds the Audio/MIDIStreaming interface (class 1,
  subclass 3), claims it (auto-detaching Android's kernel driver), reads
  the bulk IN endpoint on a thread, decodes the 4-byte USB-MIDI event
  packets (SysEx reassembled per cable) and encodes outgoing messages.
  The Exquis exposes two cables each way; like the Linux backend, writes
  go to every output cable.
- `src/exquis/midi.rs` keeps the shared types (`ExquisDevice`,
  `DeviceSelection`, `select_devices`) and re-exports `list_devices`,
  `open_inputs` and `send_to_outputs` from `usbmidi` on Android. The input
  handle type is `midi::InputConnection` on all platforms.
- Only **one Exquis** is supported on Android (one fd per `termux-usb`).

### Device access through termux-usb

Android apps may not open `/dev/bus/usb/*` directly. The Termux:API app
opens the device after the user accepts the permission dialog and passes
an already-open file descriptor to a callback command
(`termux-usb -E -e <cmd> <device>` exports it as `TERMUX_USB_FD`).

xentool makes this transparent (`usbmidi::bootstrap`, called before clap
parses anything):

1. If `XENTOOL_REEXEC_ARGS` is set we are the re-executed child: the real
   argument vector is parsed from that JSON (termux-callback only
   word-splits its command string, so arguments are never passed on the
   command line). If `XENTOOL_REEXEC_TTY` is set, stdin/stdout/stderr are
   re-attached to `/dev/tty` with `dup2`, because termux-api relays the
   callback's output through a pipe that is only flushed when the child
   exits (the TUI was invisible otherwise).
2. If `TERMUX_USB_FD` is already set, or the command does not need the
   device (`help`, `edit`, `geometry`, anything routed to the Wooting
   backend by a `.wtn` argument), run normally.
3. Otherwise run `termux-usb -l`, probe every listed device by re-running
   ourselves under `termux-usb -r -E -e <self> <path>` with the hidden
   `usb-probe` subcommand (prints `vid:pid product serial`), pick the one
   that is an Exquis (VID 2985 / PID 0007 or product string "Exquis"),
   and `exec` ourselves under `termux-usb -E -e <self> <path>`.

The xentool binary path must not contain whitespace (termux-callback
limitation). `~` is not expanded inside the callback either; the
bootstrap always uses the absolute path of the running executable.

libusb must be told not to scan the bus (`rusb::disable_device_discovery`
before the first context) and the device is opened with
`open_device_with_fd` (`libusb_wrap_sys_device`).

## 2. Synth output: OSC instead of a virtual MIDI port

There is no MIDI port for sclang to subscribe to, so `src/midi_out.rs`
gained a second transport usable on **every** platform:

```
--output osc://127.0.0.1:57120        # Android default (cli.rs)
```

Each retuned message is sent as `/xentool/midi <status> <data1> <data2>`
(ints); SysEx as `/xentool/sysex <blob>`. `supercollider/xentool_osc_midi.scd`
dispatches these through `MIDIIn.doNoteOnAction` & co, so the existing
`MIDIdef`s in the patches fire unchanged. Both patches load the bridge
(`~patchDir +/+ "xentool_osc_midi.scd"`); it is inert elsewhere.

SuperCollider-side changes that came with this:

- `MIDIClient.init` throws on Termux (no MIDI backend). Both patches wrap
  it in `try`, expose `~midiAvailable` / `~midiSources`, and never touch
  `MIDIClient.sources` directly.
- `thisProcess.nowExecutingPath` is the *relative* path given to
  `sclang file.scd`, so `pathOnly` is empty. Patches compute `~patchDir`
  with a cwd fallback (used for `logs/` and the bridge file).
- The tanpura patch now applies `tanpura_studio/presets/_default.json`
  itself at boot (31 params), so it sounds like the Pi without the Flask
  studio relay running. The relay, when present, re-sends the same values.

## 3. Audio: jack2's OpenSL ES backend crackles, PulseAudio does not

scsynth on Termux needs JACK. jack2's `opensles` driver crackled on the
Pixel 7 at **every** period size tested (240, 480, 960, 2048, 4096 frames,
with and without capture), even for a pure sine. Reading the driver
source (`android/opensl_io.c`): it keeps a single OpenSL buffer queued and
enqueues the next one only after the previous *finished*, so every period
boundary can underrun; it reports no xruns. PulseAudio's
`module-sles-sink` (already used by Termux) played the same tone cleanly.

Solution in the launcher:

```
pactl load-module module-aaudio-sink sink_name=xentool_aaudio rate=48000
jackd -d dummy -r 48000 -p 2048 -w 42666         # software clock for scsynth
pacat --raw --format=s16le --rate=48000 --channels=2 --latency-msec=20       --device=xentool_aaudio <fifo> &
jack2pulse > <fifo>                              # scripts/termux/jack2pulse.c
```

PulseAudio's default `OpenSL_ES_sink` is clean but slow: `module-sles-sink`
uses a fixed 125 ms block (`latency=` argument) and pacat's stream sat at
~140 ms on top, which made pad-to-sound lag obvious. Termux's PulseAudio
also ships `module-aaudio-sink` (Android's low-latency audio API,
`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY` by default); loaded at runtime as an
*extra* sink it measured 18 ms stream + 8 ms sink latency. No PulseAudio
config file is changed and the default sink/source stay as they are (the
TTS + microphone setup on the phone keeps working); the sink vanishes
when PulseAudio restarts and the launcher re-creates it.

`jack2pulse` is a small JACK client (built with `clang … -ljack` on first
run) that auto-connects `SuperCollider:out_1/2`, converts to interleaved
s16 and streams through a ring buffer into the FIFO. 48 kHz matches the AAudio
sink and avoids resampling (use 44.1 kHz with the OpenSL sink). The dummy backend's `-w` is its
cycle time; keep it at exactly one period.

Latency is actively bounded: the bridge shrinks the kernel pipe to one
page (`F_SETPIPE_SZ`, a 64 KB pipe alone would hold ~370 ms) and steers
the ring buffer's average fill towards one period by dropping or
duplicating a single frame per 1024 whenever the average leaves
[½ period, 2 periods]. That 0.1 % trim is inaudible and also absorbs the
long-term clock drift between the dummy backend and the OpenSL sink.

Two Termux quirks baked into the script:

- `pacat` reading **stdin** fails with "Failed to set media name" (the
  default media name goes through a locale conversion that bionic's iconv
  rejects); giving it the FIFO as a file argument works.
- No realtime scheduling is available (`Couldn't set realtime scheduling
  priority`); the 100 ms PulseAudio latency plus the ring buffer absorb
  the jitter.

Expected latency with the 2048-frame default: ~43 ms period + ~11 ms pipe
+ 20 ms Pulse target + 8 ms AAudio ≈ 80 ms. `JACK_PERIOD=1024` halves the
first term but glitched about once a second in live playing on the Pixel 7;
`PULSE_SINK=OpenSL_ES_sink` selects the default (slow) sink.

### Measuring glitches instead of listening

JACK's dummy backend reports an xrun whenever scsynth or the bridge did
not finish within a cycle; in async mode that repeats the previous
buffer, which sounds like crackle or "overdriven amp" distortion. A
reproducible test injects two-note chords with dense MPE expression over
OSC and counts xruns (`~/dev/xruntest.sh` on the phone; the bridge also
prints peak/clip counts so real clipping can be ruled out):

| configuration (AAudio sink)                 | xruns / 20 s |
|---------------------------------------------|--------------|
| 512-frame period, with TUI                  | 301          |
| 512, pinned to the fast cores               | 173          |
| 1024, with TUI                              | 61           |
| 1024, pinned / nice -15 (no real change)    | 42–50        |
| 1024, synthetic chords only                 | 0–2          |
| 1024, live playing with TUI                 | 14–16 / 35 s |
| 1024, live playing headless (`--no-tui`)    | 36–77 / 35 s |
| 2048, live playing with TUI                 | 0 / 35 s     |

Core pinning (`taskset`) and `renice` do not help: Android's foreground
cpuset only exposes two of the fast cores anyway, and the lateness hits
both clients at once, which points at the dummy driver's own timer
waking late and compressing the next cycle. What helps is less work per
cycle (the TUI redrew on every MPE message, ~30 % of a core; `--no-tui`
and the 25 fps redraw cap fix that) and a longer period.

## 4. Smaller things

- `src/hud/tui_url.rs`: clipboard via `termux-clipboard-set`, browser via
  `termux-open-url` (needs the termux-api package + app).
- `settings.json` / `devices.json` live under `$HOME/.config` and
  `$HOME/.local/share` as on Linux (the `directories` crate).
- The HUD works at `http://localhost:9099/` in the phone browser.
- Release builds matter: the debug `xentool serve` used ~32 % CPU on the
  phone, the release build ~4 % idle. Under dense MPE traffic even the
  release TUI redrew per message (~30 %); redraws are now capped at 25 fps
  and the launcher runs `serve --no-tui` by default (`XENTOOL_TUI=1` to
  get it back).
- `cargo test` passes on Termux (89 tests incl. the USB-MIDI codec tests).

## 5. Not done / known limits

- Multiple Exquis boards, MTS-ESP (`LIBMTS.dll` is Windows only), the
  Wooting backend, the xenharm sidecar and the studio web UIs were not
  tried on Android.
- The USB permission is per plug-in; Android forgets it when the Exquis
  is unplugged, and `termux-usb` blocks until the dialog is answered.
