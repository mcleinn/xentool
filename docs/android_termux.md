# xentool on Android / Termux

Status: **experimental, Exquis flow only.** Brought up on a Pixel 7
(Android 14, Termux 2026.10 from Google Play, Termux:API app installed)
with the Exquis plugged straight into USB-C. Wooting is not attempted
(no Analog/RGB SDK for Android).

This document lists everything that differs from the Linux (Pi) and
Windows setups, why, and how to run it. The short version:

```bash
pkg install rust git clang pkg-config libusb termux-api supercollider jack2 pulseaudio tmux
git clone https://github.com/mcleinn/xentool.git ~/dev/xentool
cd ~/dev/xentool && cargo build --release
scripts/run-termux-exquis.sh xtn/edo31.xtn
```

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
jackd -d dummy -r 44100 -p 512 -w 11610          # software clock for scsynth
pacat --raw --format=s16le --rate=44100 --channels=2 --latency-msec=40 <fifo> &
jack2pulse > <fifo>                              # scripts/termux/jack2pulse.c
```

`jack2pulse` is a small JACK client (built with `clang … -ljack` on first
run) that auto-connects `SuperCollider:out_1/2`, converts to interleaved
s16 and streams through a ring buffer into the FIFO. 44.1 kHz matches the
PulseAudio sink and avoids resampling. The dummy backend's `-w` is its
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

Expected latency: ~12 ms period + ~12 ms pipe + 40 ms Pulse target plus the
sink's own buffer. Raise `PULSE_LATENCY_MS` / `JACK_PERIOD` if it crackles,
lower them if the phone keeps up.

## 4. Smaller things

- `src/hud/tui_url.rs`: clipboard via `termux-clipboard-set`, browser via
  `termux-open-url` (needs the termux-api package + app).
- `settings.json` / `devices.json` live under `$HOME/.config` and
  `$HOME/.local/share` as on Linux (the `directories` crate).
- The HUD works at `http://localhost:9099/` in the phone browser.
- Release builds matter: the debug `xentool serve` used ~32 % CPU on the
  phone (TUI redraw), the release build ~4 %.
- `cargo test` passes on Termux (89 tests incl. the USB-MIDI codec tests).

## 5. Not done / known limits

- Multiple Exquis boards, MTS-ESP (`LIBMTS.dll` is Windows only), the
  Wooting backend, the xenharm sidecar and the studio web UIs were not
  tried on Android.
- The USB permission is per plug-in; Android forgets it when the Exquis
  is unplugged, and `termux-usb` blocks until the dialog is answered.
