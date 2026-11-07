//! openxen analog stream, protocol version 1 (openxen repo: `docs/stream-protocol.md`).
//!
//! One 144-byte HID input report per completed scan, little endian:
//! ```text
//!  0 u16 magic 0x584F ("OX")   2 u8 version (1)        3 u8 flags
//!  4 u16 sequence (wraps)      6 u16 dropped (firmware) 8 u32 timestamp_us (wraps)
//! 12 u8 num_keys (56)         13 u8 cal_first_key     14 u8 adc_bits (12)  15 reserved
//! 16 u16 values[56]           128 {u16 rest, u16 bottom_out}[4] for keys cal_first_key..+3
//! ```

use anyhow::{Result, bail};

pub const MAGIC: u16 = 0x584F;
pub const VERSION: u8 = 1;
pub const USAGE_PAGE: u16 = 0xFFAC;
pub const REPORT_SIZE: usize = 144;
pub const MAX_KEYS: usize = 56;
pub const CAL_PER_REPORT: usize = 4;
#[allow(dead_code)]
pub const FLAG_INVERTED: u8 = 0x01;
#[allow(dead_code)] // reported by the firmware; not used by the key logic yet
pub const FLAG_CALIBRATING: u8 = 0x02;

/// All fields of a report; the serve path uses values/sequence/time/calibration, the rest
/// is kept for diagnostics and tests.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Report {
    pub flags: u8,
    pub sequence: u16,
    pub dropped: u16,
    pub timestamp_us: u32,
    pub num_keys: u8,
    pub cal_first_key: u8,
    pub adc_bits: u8,
    pub values: [u16; MAX_KEYS],
    pub cal: [(u16, u16); CAL_PER_REPORT],
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

/// Parse one report. Accepts a leading 0 report-ID byte (some HID backends keep it).
pub fn parse(raw: &[u8]) -> Result<Report> {
    let b = if raw.len() == REPORT_SIZE + 1 && raw[0] == 0 { &raw[1..] } else { raw };
    if b.len() != REPORT_SIZE {
        bail!("openxen report must be {REPORT_SIZE} bytes, got {}", b.len());
    }
    let magic = u16_at(b, 0);
    if magic != MAGIC || b[2] != VERSION {
        bail!("not an openxen stream report (magic {magic:#06x}, version {})", b[2]);
    }
    let num_keys = b[12];
    if num_keys == 0 || num_keys as usize > MAX_KEYS {
        bail!("bad num_keys {num_keys}");
    }
    let mut values = [0u16; MAX_KEYS];
    for (k, v) in values.iter_mut().enumerate() {
        *v = u16_at(b, 16 + 2 * k);
    }
    let mut cal = [(0u16, 0u16); CAL_PER_REPORT];
    for (i, c) in cal.iter_mut().enumerate() {
        *c = (u16_at(b, 128 + 4 * i), u16_at(b, 130 + 4 * i));
    }
    Ok(Report {
        flags: b[3],
        sequence: u16_at(b, 4),
        dropped: u16_at(b, 6),
        timestamp_us: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
        num_keys,
        cal_first_key: b[13],
        adc_bits: b[14],
        values,
        cal,
    })
}

/// One decoded scan of one module.
#[allow(dead_code)] // sequence/flags: diagnostics and tests
#[derive(Debug, Clone)]
pub struct Frame {
    /// Unwrapped sequence number.
    pub sequence: u64,
    /// Unwrapped device time, microseconds.
    pub time_us: u64,
    /// Scans lost between the previous frame and this one.
    pub lost_before: u64,
    pub num_keys: u8,
    /// Key travel 0..1 (rest..bottom-out); NaN while the key's calibration is unknown.
    pub travel: [f32; MAX_KEYS],
    pub flags: u8,
}

/// Per-module decoder: unwraps sequence/time, counts losses, keeps the rotating
/// calibration table and converts raw values to travel.
#[derive(Debug, Clone)]
pub struct Decoder {
    rest: [u16; MAX_KEYS],
    bottom: [u16; MAX_KEYS],
    known: [bool; MAX_KEYS],
    seq: Option<u64>,
    time: u64,
    pub lost_total: u64,
    pub frames: u64,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            rest: [0; MAX_KEYS],
            bottom: [0; MAX_KEYS],
            known: [false; MAX_KEYS],
            seq: None,
            time: 0,
            lost_total: 0,
            frames: 0,
        }
    }
}

impl Decoder {
    #[allow(dead_code)] // tests and diagnostics
    pub fn calibration(&self, key: usize) -> Option<(u16, u16)> {
        (key < MAX_KEYS && self.known[key]).then(|| (self.rest[key], self.bottom[key]))
    }

    pub fn feed(&mut self, raw: &[u8]) -> Result<Frame> {
        let r = parse(raw)?;
        let n = r.num_keys as usize;
        for (i, &(rest, bottom)) in r.cal.iter().enumerate() {
            let k = (r.cal_first_key as usize + i) % n;
            self.rest[k] = rest;
            self.bottom[k] = bottom;
            self.known[k] = true;
        }
        let (seq, lost, time) = match self.seq {
            None => (r.sequence as u64, 0, r.timestamp_us as u64),
            Some(prev) => {
                let step = (r.sequence as u64).wrapping_sub(prev) & 0xFFFF;
                if step == 0 {
                    bail!("repeated sequence number {}", r.sequence);
                }
                let dt = (r.timestamp_us as u64).wrapping_sub(self.time) & 0xFFFF_FFFF;
                (prev + step, step - 1, self.time + dt)
            }
        };
        self.seq = Some(seq);
        self.time = time;
        self.lost_total += lost;
        self.frames += 1;
        let mut travel = [f32::NAN; MAX_KEYS];
        for k in 0..n {
            if self.known[k] && self.bottom[k] > self.rest[k] {
                let t = (r.values[k] as f32 - self.rest[k] as f32) / (self.bottom[k] - self.rest[k]) as f32;
                travel[k] = t.clamp(0.0, 1.0);
            }
        }
        Ok(Frame { sequence: seq, time_us: time, lost_before: lost, num_keys: r.num_keys, travel, flags: r.flags })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports produced by the openxen firmware's own packing code (see the header of
    /// the fixture files).
    fn fixture(text: &str) -> Vec<Vec<u8>> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| (0..l.len()).step_by(2).map(|i| u8::from_str_radix(&l[i..i + 2], 16).unwrap()).collect())
            .collect()
    }

    fn expected_value(scan: u64, k: usize) -> u16 {
        ((scan * 100 + k as u64) & 0x0FFF) as u16
    }

    #[test]
    fn decodes_firmware_reports() {
        let reports = fixture(include_str!("testdata/stream_basic.hex"));
        assert_eq!(reports.len(), 14);
        let mut d = Decoder::default();
        for (i, raw) in reports.iter().enumerate() {
            let r = parse(raw).unwrap();
            assert_eq!(r.values[7], expected_value(i as u64, 7));
            let f = d.feed(raw).unwrap();
            assert_eq!(f.sequence, i as u64);
            assert_eq!(f.time_us, 1000 + 250 * i as u64);
            assert_eq!(f.lost_before, 0);
            assert_eq!(f.flags, FLAG_INVERTED);
        }
        // the harness reports rest = 1000 + k, bottom = 3000 + k for every key
        for k in 0..56 {
            assert_eq!(d.calibration(k), Some((1000 + k as u16, 3000 + k as u16)));
        }
        let last = d.feed_check_last(&reports);
        let k = 5;
        let want = ((expected_value(13, k) as f32 - (1000 + k) as f32) / 2000.0).clamp(0.0, 1.0);
        assert!((last.travel[k] - want).abs() < 1e-6);
    }

    #[test]
    fn overflow_shows_as_lost_scans_and_time_unwraps() {
        let reports = fixture(include_str!("testdata/stream_overflow.hex"));
        let mut d = Decoder::default();
        let frames: Vec<Frame> = reports.iter().map(|r| d.feed(r).unwrap()).collect();
        let seqs: Vec<u64> = frames.iter().map(|f| f.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5, 6, 7, 12, 13, 14]);
        assert_eq!(frames[8].lost_before, 4);
        assert_eq!(d.lost_total, 4);
        assert!(frames.windows(2).all(|w| w[1].time_us > w[0].time_us));
        assert_eq!(frames.last().unwrap().time_us - frames[0].time_us, 14 * 250);
    }

    #[test]
    fn rejects_garbage_and_accepts_report_id_prefix() {
        assert!(parse(&[0u8; REPORT_SIZE]).is_err());
        let reports = fixture(include_str!("testdata/stream_basic.hex"));
        let mut with_id = vec![0u8];
        with_id.extend_from_slice(&reports[0]);
        assert_eq!(parse(&with_id).unwrap().sequence, 0);
    }

    #[test]
    fn repeated_sequence_is_an_error() {
        let reports = fixture(include_str!("testdata/stream_basic.hex"));
        let mut d = Decoder::default();
        d.feed(&reports[0]).unwrap();
        assert!(d.feed(&reports[0]).is_err());
    }

    impl Decoder {
        fn feed_check_last(&self, reports: &[Vec<u8>]) -> Frame {
            let mut d = Decoder::default();
            let mut last = None;
            for r in reports {
                last = Some(d.feed(r).unwrap());
            }
            last.unwrap()
        }
    }
}
