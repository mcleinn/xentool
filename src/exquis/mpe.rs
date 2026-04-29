use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::exquis::proto::{control_display_name, control_name};

#[derive(Debug, Clone)]
pub struct InputMessage {
    pub _timestamp: u64,
    pub device_number: usize,
    pub port_name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub ts: String,
    pub device: usize,
    pub port: String,
    pub channel: Option<u8>,
    pub kind: String,
    pub note: Option<u8>,
    pub value: Option<i16>,
    pub label: Option<String>,
    pub raw: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct DisplayEvent {
    pub line: String,
    pub record: EventRecord,
}

#[derive(Debug, Clone)]
pub struct TouchSummary {
    pub device: usize,
    pub channel: u8,
    pub note: u8,
    pub velocity: u8,
    pub x: i16,
    pub y: u8,
    pub z: u8,
    pub age: Duration,
    /// Optional exact frequency (Hz) when tuning state is attached.
    pub freq_hz: Option<f64>,
    /// Virtual channel from .xtn (Chan_N).
    pub v_chan: Option<u8>,
    /// Virtual note/key from .xtn (Key_N).
    pub v_key: Option<u8>,
    /// Absolute pitch in EDO steps.
    pub abs_pitch: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct DecodedEvent {
    pub raw: InputMessage,
    pub events: Vec<DisplayEvent>,
    pub touches: Vec<TouchSummary>,
}

impl DecodedEvent {
    pub fn raw_line(&self) -> String {
        format!(
            "[{}] raw {}",
            self.raw.device_number,
            self.raw
                .bytes
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    pub fn event_lines(&self, mpe_only: bool) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| !mpe_only || event.record.is_mpe_related())
            .map(|event| event.line.clone())
            .collect()
    }

    pub fn records(&self, log_raw: bool) -> Vec<EventRecord> {
        let mut records = self
            .events
            .iter()
            .map(|event| event.record.clone())
            .collect::<Vec<_>>();
        if records.is_empty() || log_raw {
            records.push(EventRecord {
                ts: now_rfc3339(),
                device: self.raw.device_number,
                port: self.raw.port_name.clone(),
                channel: None,
                kind: "raw".to_string(),
                note: None,
                value: None,
                label: None,
                raw: Some(self.raw.bytes.clone()),
            });
        }
        records
    }
}

impl EventRecord {
    pub fn is_mpe_related(&self) -> bool {
        matches!(self.kind.as_str(), "note_on" | "note_off" | "x" | "y" | "z")
    }
}

/// Tracks the current state of non-musical controls (encoders, buttons, slider).
/// Populated from channel-16 messages when dev mode is active for those zones.
#[derive(Debug, Clone, Default)]
pub struct ControlStateTracker {
    /// Encoder accumulated positions (relative deltas summed). Key = control id (110-113).
    pub encoders: BTreeMap<u8, i64>,
    /// Button pressed state. Key = control id (100-109, 114-118).
    pub buttons: BTreeMap<u8, bool>,
    /// Slider touched portion (80-85) or position (90). 127 = untouched.
    pub slider_portion: Option<u8>,
    /// Last seen slider position value (CC 90).
    pub slider_position: Option<u8>,
}

impl ControlStateTracker {
    /// Update state from a raw MIDI message. Returns true if state changed.
    pub fn apply(&mut self, msg: &[u8]) -> bool {
        if msg.len() < 3 {
            return false;
        }
        let status = msg[0] & 0xF0;
        let channel = (msg[0] & 0x0F) + 1; // 1-16
        if channel != 16 {
            return false;
        }
        match status {
            // CC on channel 16: encoders, buttons, slider
            0xB0 => {
                let controller = msg[1];
                let value = msg[2];
                match controller {
                    110..=113 => {
                        let delta = value as i16 - 64;
                        *self.encoders.entry(controller).or_insert(0) += delta as i64;
                        true
                    }
                    100..=109 | 114..=118 => {
                        self.buttons.insert(controller, value != 0);
                        true
                    }
                    80..=85 => {
                        self.slider_portion = Some(controller - 80);
                        true
                    }
                    90 => {
                        self.slider_position = Some(value);
                        if value == 127 {
                            self.slider_portion = None;
                        }
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

#[derive(Default)]
pub struct Decoder {
    active: BTreeMap<(usize, u8, u8), TouchState>,
}

#[derive(Debug, Clone)]
struct TouchState {
    device: usize,
    channel: u8,
    note: u8,
    velocity: u8,
    x: i16,
    y: u8,
    z: u8,
    started_at: Instant,
}

impl Decoder {
    /// Drop all currently-tracked active touches. Called from layout-cycle
    /// paths in `cmd_serve` so the HUD doesn't hold phantom held-note state
    /// after the host issues an all-notes-off (CC 123) broadcast — the
    /// Exquis itself won't fire note_offs unless the user lifts their finger.
    pub fn clear(&mut self) {
        self.active.clear();
    }

    pub fn process(&mut self, raw: InputMessage) -> DecodedEvent {
        let mut events = Vec::new();
        if raw.bytes.is_empty() {
            return self.finish(raw, events);
        }

        let status = raw.bytes[0];
        let channel = (status & 0x0F) + 1;
        match status & 0xF0 {
            0x80 => {
                if raw.bytes.len() >= 3 {
                    self.active
                        .remove(&(raw.device_number, channel, raw.bytes[1]));
                    events.push(event(
                        &raw,
                        Some(channel),
                        "note_off",
                        Some(raw.bytes[1]),
                        Some(raw.bytes[2] as i16),
                        None,
                        format!(
                            "[{}] #{} note_off note={} release={}",
                            raw.device_number, channel, raw.bytes[1], raw.bytes[2]
                        ),
                    ));
                }
            }
            0x90 => {
                if raw.bytes.len() >= 3 {
                    if raw.bytes[2] == 0 {
                        self.active
                            .remove(&(raw.device_number, channel, raw.bytes[1]));
                        events.push(event(
                            &raw,
                            Some(channel),
                            "note_off",
                            Some(raw.bytes[1]),
                            Some(0),
                            None,
                            format!(
                                "[{}] #{} note_off note={}",
                                raw.device_number, channel, raw.bytes[1]
                            ),
                        ));
                    } else {
                        self.active.insert(
                            (raw.device_number, channel, raw.bytes[1]),
                            TouchState {
                                device: raw.device_number,
                                channel,
                                note: raw.bytes[1],
                                velocity: raw.bytes[2],
                                x: 0,
                                y: 0,
                                z: 0,
                                started_at: Instant::now(),
                            },
                        );
                        let label = if channel == 16 {
                            control_name(raw.bytes[1])
                        } else {
                            None
                        };
                        let display_label = if channel == 16 {
                            control_display_name(raw.bytes[1])
                        } else {
                            None
                        };
                        events.push(event(
                            &raw,
                            Some(channel),
                            "note_on",
                            Some(raw.bytes[1]),
                            Some(raw.bytes[2] as i16),
                            label,
                            if let Some(name) = display_label {
                                format!("[{}] #{} {} pressed", raw.device_number, channel, name)
                            } else {
                                format!(
                                    "[{}] #{} note_on note={} vel={}",
                                    raw.device_number, channel, raw.bytes[1], raw.bytes[2]
                                )
                            },
                        ));
                    }
                }
            }
            0xB0 => {
                if raw.bytes.len() >= 3 {
                    let controller = raw.bytes[1];
                    let value = raw.bytes[2];
                    if channel == 16 && (80..=117).contains(&controller) {
                        let label = control_name(controller);
                        let display = control_display_name(controller)
                            .unwrap_or_else(|| format!("Control {controller}"));
                        let line = if (110..=113).contains(&controller) {
                            format!(
                                "[{}] #{} {} delta={:+}",
                                raw.device_number,
                                channel,
                                display,
                                value as i16 - 64
                            )
                        } else if controller == 90 {
                            format!(
                                "[{}] #{} {} portion={}",
                                raw.device_number, channel, display, value
                            )
                        } else {
                            format!(
                                "[{}] #{} {} {}",
                                raw.device_number,
                                channel,
                                display,
                                if value == 0 { "released" } else { "pressed" }
                            )
                        };
                        events.push(event(
                            &raw,
                            Some(channel),
                            "control",
                            None,
                            Some(value as i16),
                            label,
                            line,
                        ));
                    } else if controller == 74 {
                        for touch in self.active.values_mut().filter(|touch| {
                            touch.device == raw.device_number && touch.channel == channel
                        }) {
                            touch.y = value;
                        }
                        events.push(event(
                            &raw,
                            Some(channel),
                            "y",
                            None,
                            Some(value as i16),
                            Some("cc74".to_string()),
                            format!("[{}] #{} y={} (cc74)", raw.device_number, channel, value),
                        ));
                    } else {
                        events.push(event(
                            &raw,
                            Some(channel),
                            "cc",
                            None,
                            Some(value as i16),
                            Some(format!("cc_{controller}")),
                            format!(
                                "[{}] #{} cc {} value={}",
                                raw.device_number, channel, controller, value
                            ),
                        ));
                    }
                }
            }
            0xD0 => {
                if raw.bytes.len() >= 2 {
                    for touch in self.active.values_mut().filter(|touch| {
                        touch.device == raw.device_number && touch.channel == channel
                    }) {
                        touch.z = raw.bytes[1];
                    }
                    events.push(event(
                        &raw,
                        Some(channel),
                        "z",
                        None,
                        Some(raw.bytes[1] as i16),
                        Some("channel_pressure".to_string()),
                        format!(
                            "[{}] #{} z={} (channel pressure)",
                            raw.device_number, channel, raw.bytes[1]
                        ),
                    ));
                }
            }
            0xA0 => {
                if raw.bytes.len() >= 3 {
                    if let Some(touch) =
                        self.active
                            .get_mut(&(raw.device_number, channel, raw.bytes[1]))
                    {
                        touch.z = raw.bytes[2];
                    }
                    if channel == 16 {
                        let label = control_name(raw.bytes[1]);
                        let display = control_display_name(raw.bytes[1])
                            .unwrap_or_else(|| format!("Control {}", raw.bytes[1]));
                        events.push(event(
                            &raw,
                            Some(channel),
                            "effect",
                            Some(raw.bytes[1]),
                            Some(raw.bytes[2] as i16),
                            label,
                            format!(
                                "[{}] #{} {} fx={}",
                                raw.device_number, channel, display, raw.bytes[2]
                            ),
                        ));
                    } else {
                        events.push(event(
                            &raw,
                            Some(channel),
                            "z",
                            Some(raw.bytes[1]),
                            Some(raw.bytes[2] as i16),
                            Some("poly_aftertouch".to_string()),
                            format!(
                                "[{}] #{} z={} note={} (poly aftertouch)",
                                raw.device_number, channel, raw.bytes[2], raw.bytes[1]
                            ),
                        ));
                    }
                }
            }
            0xE0 => {
                if raw.bytes.len() >= 3 {
                    let value = (((raw.bytes[2] as i16) << 7) | raw.bytes[1] as i16) - 8192;
                    for touch in self.active.values_mut().filter(|touch| {
                        touch.device == raw.device_number && touch.channel == channel
                    }) {
                        touch.x = value;
                    }
                    events.push(event(
                        &raw,
                        Some(channel),
                        "x",
                        None,
                        Some(value),
                        Some("pitch_bend".to_string()),
                        format!(
                            "[{}] #{} x={:+} (pitch bend)",
                            raw.device_number, channel, value
                        ),
                    ));
                }
            }
            _ => {}
        }

        self.finish(raw, events)
    }

    fn finish(&self, raw: InputMessage, events: Vec<DisplayEvent>) -> DecodedEvent {
        let mut touches = self
            .active
            .values()
            .filter(|touch| touch.device == raw.device_number)
            .map(|touch| TouchSummary {
                device: touch.device,
                channel: touch.channel,
                note: touch.note,
                velocity: touch.velocity,
                x: touch.x,
                y: touch.y,
                z: touch.z,
                age: touch.started_at.elapsed(),
                freq_hz: None,
                v_chan: None,
                v_key: None,
                abs_pitch: None,
            })
            .collect::<Vec<_>>();
        touches.sort_by_key(|touch| (touch.device, touch.channel, touch.note));
        DecodedEvent {
            raw,
            events,
            touches,
        }
    }
}

fn event(
    raw: &InputMessage,
    channel: Option<u8>,
    kind: &str,
    note: Option<u8>,
    value: Option<i16>,
    label: Option<String>,
    line: String,
) -> DisplayEvent {
    DisplayEvent {
        line,
        record: EventRecord {
            ts: now_rfc3339(),
            device: raw.device_number,
            port: raw.port_name.clone(),
            channel,
            kind: kind.to_string(),
            note,
            value,
            label,
            raw: None,
        },
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[derive(Default)]
pub struct EventBuffer {
    entries: VecDeque<String>,
}

impl EventBuffer {
    pub fn push(&mut self, line: String) {
        self.entries.push_back(line);
        while self.entries.len() > 200 {
            self.entries.pop_front();
        }
    }

    pub fn entries(&self) -> &VecDeque<String> {
        &self.entries
    }
}
