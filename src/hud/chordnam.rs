//! Chord-name database derived from Manuel Op de Coul's `chordnam.par`
//! (Scala). Embedded into the binary via `include_bytes!` so xentool stays
//! self-sufficient — no data files to ship.
//!
//! Direct port of `webconfigurator/server/chords.js` from the predecessor
//! `xenwooting` project. Three lookup tables are populated:
//!
//! - `by_edo_exact` — step patterns under `<SCALA_SCALE_DEF 2^(1/N)>` blocks
//!   in the file (canonical names for that EDO).
//! - `embed12_by_edo` — for EDOs divisible by 12, the 12-EDO patterns scaled
//!   up by the multiplier (`-EDO12`-suffixed names).
//! - `approx_by_edo` — tuning-independent templates (ratios / cents) projected
//!   to the target EDO by rounding to the nearest step. Templates with a
//!   per-pitch rounding error >15 c are dropped; >3 c gets a `~Nc` suffix.
//!
//! Both `embed12_by_edo` and `approx_by_edo` are computed lazily per EDO on
//! first request. Lookup is cheap once cached.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;

const CHORDNAM_PAR: &str = include_str!("../../assets/chordnam.par");

/// Templates whose worst per-pitch rounding error exceeds this are dropped
/// for a given EDO (in cents).
const APPROX_MAX_ERR_CENTS: f64 = 15.0;

/// Templates whose error exceeds this get a `~Nc` cents-error suffix on
/// their name (in cents).
const APPROX_SHOW_ERR_OVER_CENTS: f64 = 3.0;

#[derive(Debug, Clone, Serialize)]
pub struct ChordResult {
    #[serde(rename = "rootPc")]
    pub root_pc: i32,
    pub rel: Vec<i32>,
    pub pattern: String,
    pub names: Vec<String>,
}

#[derive(Debug, Clone)]
struct Template {
    cents: Vec<f64>,
    name: String,
}

pub struct ScalaChordDb {
    /// edo → pattern (e.g. `"4-3-3"`) → list of chord names.
    by_edo_exact: HashMap<i32, HashMap<String, Vec<String>>>,
    /// Tuning-independent templates parsed once at load time.
    templates: Vec<Template>,
    /// Lazily-filled per-EDO projection of `templates`.
    approx_by_edo: Mutex<HashMap<i32, HashMap<String, Vec<String>>>>,
    /// Lazily-filled per-EDO embedding of 12-EDO patterns into multiples of 12.
    embed12_by_edo: Mutex<HashMap<i32, HashMap<String, Vec<String>>>>,
}

impl ScalaChordDb {
    /// Ensure the embed12 + approx tables are populated for the given EDO.
    /// Idempotent.
    fn ensure_for_edo(&self, edo: i32) {
        {
            let mut approx = self.approx_by_edo.lock().unwrap();
            if !approx.contains_key(&edo) {
                let mut m: HashMap<String, Vec<String>> = HashMap::new();
                for tpl in &self.templates {
                    let proj = project_template_to_edo(&tpl.cents, edo);
                    if proj.pattern.is_empty() {
                        continue;
                    }
                    if proj.max_abs_err_cents > APPROX_MAX_ERR_CENTS {
                        continue;
                    }
                    let name = with_err_suffix(&tpl.name, proj.max_abs_err_cents);
                    let entry = m.entry(proj.pattern.clone()).or_default();
                    if !entry.iter().any(|n| n == &name) {
                        entry.push(name);
                    }
                }
                approx.insert(edo, m);
            }
        }

        {
            let mut embed = self.embed12_by_edo.lock().unwrap();
            if !embed.contains_key(&edo) {
                let mut m: HashMap<String, Vec<String>> = HashMap::new();
                if edo != 12 && edo % 12 == 0 {
                    if let Some(patterns12) = self.by_edo_exact.get(&12) {
                        let f = edo / 12;
                        for (pat12, names12) in patterns12 {
                            let steps12: Vec<i32> = pat12
                                .split('-')
                                .filter_map(|s| s.parse::<i32>().ok())
                                .filter(|n| *n > 0)
                                .collect();
                            if steps12.is_empty() {
                                continue;
                            }
                            let pat_n: String = steps12
                                .iter()
                                .map(|s| (s * f).to_string())
                                .collect::<Vec<_>>()
                                .join("-");
                            let out_names: Vec<String> = names12
                                .iter()
                                .filter(|n| !n.is_empty())
                                .map(|n| format!("{}-EDO12", n))
                                .collect();
                            let entry = m.entry(pat_n).or_default();
                            for nm in out_names {
                                if !entry.iter().any(|n| n == &nm) {
                                    entry.push(nm);
                                }
                            }
                        }
                    }
                }
                embed.insert(edo, m);
            }
        }
    }
}

fn mod_pos(n: i32, m: i32) -> i32 {
    let x = n % m;
    if x < 0 { x + m } else { x }
}

fn norm_pc(edo: i32, pc: i32) -> i32 {
    mod_pos(pc, edo)
}

fn uniq_sorted(nums: impl IntoIterator<Item = i32>) -> Vec<i32> {
    let mut v: Vec<i32> = nums.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn step_pattern_from_rel_pcs(rel: &[i32]) -> String {
    if rel.len() < 2 {
        return String::new();
    }
    let mut steps: Vec<String> = Vec::with_capacity(rel.len() - 1);
    for i in 1..rel.len() {
        steps.push((rel[i] - rel[i - 1]).to_string());
    }
    steps.join("-")
}

fn rel_pcs_from_step_pattern(edo: i32, pattern: &str) -> Vec<i32> {
    let steps: Vec<i32> = pattern
        .split('-')
        .filter_map(|s| s.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .collect();
    if steps.is_empty() {
        return Vec::new();
    }
    let mut rel: Vec<i32> = vec![0];
    let mut acc = 0i32;
    for s in steps {
        acc += s;
        rel.push(norm_pc(edo, acc));
    }
    uniq_sorted(rel)
}

fn step_pattern_from_pcs(edo: i32, root_pc: i32, pcs: &[i32]) -> (Vec<i32>, String) {
    let rel: Vec<i32> = pcs.iter().map(|pc| norm_pc(edo, pc - root_pc)).collect();
    let uniq = uniq_sorted(rel);
    if uniq.is_empty() || uniq[0] != 0 {
        return (uniq, String::new());
    }
    if uniq.len() == 1 {
        return (uniq, String::new());
    }
    let pattern = step_pattern_from_rel_pcs(&uniq);
    (uniq, pattern)
}

#[derive(Debug, Clone)]
struct Ratio {
    num: u64,
    den: u64,
}

fn parse_ratio_token(tok: &str) -> Option<Ratio> {
    let s = tok.trim();
    if s.is_empty() {
        return None;
    }
    // Strip surrounding parens (e.g. `(1:2:3:4)`); they're allowed by the
    // file format around the entire chord def, not on individual tokens.
    let s = s.trim_start_matches('(').trim_end_matches(')');
    if let Some((a, b)) = s.split_once('/') {
        let a: u64 = a.parse().ok()?;
        let b: u64 = b.parse().ok()?;
        if a == 0 || b == 0 {
            return None;
        }
        Some(Ratio { num: a, den: b })
    } else {
        let a: u64 = s.parse().ok()?;
        if a == 0 {
            return None;
        }
        Some(Ratio { num: a, den: 1 })
    }
}

fn ratio_to_cents(r: f64) -> f64 {
    1200.0 * r.log2()
}

fn parse_chord_template_line(raw: &str) -> Option<Template> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('!') || line.starts_with('<') {
        return None;
    }
    // EDO step patterns are handled elsewhere (`\d+(-\d+)+ name`).
    if is_step_pattern_line(line) {
        return None;
    }

    // Absolute ratio list `4:5:6 name` (single-space sep allowed).
    if line.contains(':') {
        let mut parts = line.splitn(2, char::is_whitespace);
        let def = parts.next()?;
        let name = parts.next()?.trim();
        let name = strip_eq_prefix(name);
        let stripped_def = def.trim_start_matches('(').trim_end_matches(')');
        let toks: Vec<Ratio> = stripped_def
            .split(':')
            .filter_map(parse_ratio_token)
            .collect();
        if toks.len() < 2 {
            return None;
        }
        let root = (toks[0].num as f64) / (toks[0].den as f64);
        let mut cents = vec![0.0];
        for r in &toks[1..] {
            let v = (r.num as f64) / (r.den as f64) / root;
            cents.push(mod_f64(ratio_to_cents(v), 1200.0));
        }
        let cents = uniq_sorted_f64(cents);
        return Some(Template { cents, name: name.to_string() });
    }

    // Relative cents/ratio list (def and name separated by 2+ spaces).
    let m = match split_two_spaces(line) {
        Some(v) => v,
        None => return None,
    };
    let def = m.0.trim();
    let name = strip_eq_prefix(m.1.trim());
    let parts: Vec<&str> = def.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let has_dot = parts.iter().any(|p| p.contains('.'));
    let mut cents = vec![0.0];
    if has_dot {
        let mut acc = 0.0;
        for p in &parts {
            let v: f64 = p.parse().ok()?;
            acc += v;
            cents.push(mod_f64(acc, 1200.0));
        }
    } else {
        let mut acc = 1.0_f64;
        for p in &parts {
            let r = parse_ratio_token(p)?;
            acc *= (r.num as f64) / (r.den as f64);
            cents.push(mod_f64(ratio_to_cents(acc), 1200.0));
        }
    }
    let cents = uniq_sorted_f64(cents);
    Some(Template { cents, name: name.to_string() })
}

fn strip_eq_prefix(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix('=') {
        rest.trim_start()
    } else {
        s
    }
}

fn split_two_spaces(line: &str) -> Option<(&str, &str)> {
    // Find the first run of >= 2 whitespace chars. Mirror of JS regex
    // `^(.+?)\s{2,}(.+)$`.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b' ' || c == b'\t' {
            let start = i;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            if i - start >= 2 {
                let def = &line[..start];
                let name = &line[i..];
                if !def.is_empty() && !name.is_empty() {
                    return Some((def, name));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn is_step_pattern_line(line: &str) -> bool {
    // `^\d+(?:-\d+)+\s+name`: at least two numeric segments joined by '-',
    // followed by whitespace.
    let mut chars = line.chars().peekable();
    let mut saw_digit = false;
    let mut dash_seen = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            saw_digit = true;
            chars.next();
        } else if c == '-' {
            if !saw_digit {
                return false;
            }
            dash_seen = true;
            chars.next();
            saw_digit = false;
        } else if c.is_whitespace() {
            return saw_digit && dash_seen;
        } else {
            return false;
        }
    }
    false
}

fn mod_f64(n: f64, m: f64) -> f64 {
    let x = n % m;
    if x < 0.0 { x + m } else { x }
}

fn uniq_sorted_f64(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    v
}

struct Projection {
    pattern: String,
    max_abs_err_cents: f64,
}

fn project_template_to_edo(cents_list: &[f64], edo: i32) -> Projection {
    if edo <= 0 {
        return Projection { pattern: String::new(), max_abs_err_cents: 0.0 };
    }
    let step_cents = 1200.0 / (edo as f64);
    let mut pcs: Vec<i32> = Vec::with_capacity(cents_list.len());
    let mut max_abs_err = 0.0_f64;
    for &c in cents_list {
        let cents = mod_f64(c, 1200.0);
        let k = (cents / step_cents).round() as i64;
        let realized = (k as f64) * step_cents;
        let err = (realized - cents).abs();
        if err > max_abs_err {
            max_abs_err = err;
        }
        pcs.push(norm_pc(edo, k as i32));
    }
    let mut rel = uniq_sorted(pcs);
    if rel.is_empty() || rel[0] != 0 {
        rel.insert(0, 0);
    }
    let pattern = step_pattern_from_rel_pcs(&rel);
    Projection { pattern, max_abs_err_cents: max_abs_err }
}

fn with_err_suffix(name: &str, max_abs_err_cents: f64) -> String {
    if !max_abs_err_cents.is_finite() || max_abs_err_cents <= APPROX_SHOW_ERR_OVER_CENTS {
        return name.to_string();
    }
    let v = max_abs_err_cents.round() as i32;
    format!("{name}~{v}c")
}

pub fn parse_chordnam(text: &str) -> ScalaChordDb {
    let mut by_edo_exact: HashMap<i32, HashMap<String, Vec<String>>> = HashMap::new();
    let mut templates: Vec<Template> = Vec::new();
    let mut current_edo: Option<i32> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('!') {
            continue;
        }

        // <SCALA_SCALE_DEF 2^(1/N)> sets current EDO.
        if let Some(m) = line.strip_prefix("<SCALA_SCALE_DEF 2^(1/") {
            if let Some(num) = m.strip_suffix(")>") {
                if let Ok(e) = num.parse::<i32>() {
                    if e > 0 {
                        current_edo = Some(e);
                        by_edo_exact.entry(e).or_default();
                    } else {
                        current_edo = None;
                    }
                    continue;
                }
            }
            current_edo = None;
            continue;
        }

        // Other directives: skip.
        if line.starts_with('<') {
            continue;
        }

        // EDO step-pattern under an active <SCALA_SCALE_DEF>.
        if let Some(edo) = current_edo {
            if is_step_pattern_line(line) {
                let mut parts = line.splitn(2, char::is_whitespace);
                let pattern = parts.next().unwrap_or("").to_string();
                let rest = parts.next().unwrap_or("").trim();
                let name = strip_eq_prefix(rest);
                if !name.is_empty() {
                    let entry = by_edo_exact
                        .entry(edo)
                        .or_default()
                        .entry(pattern)
                        .or_default();
                    if !entry.iter().any(|n| n == name) {
                        entry.push(name.to_string());
                    }
                }
                continue;
            }
        }

        // Tuning-independent template (applies to all EDOs via projection).
        if let Some(tpl) = parse_chord_template_line(raw) {
            if tpl.cents.len() >= 2 && !tpl.name.is_empty() {
                templates.push(tpl);
            }
        }
    }

    ScalaChordDb {
        by_edo_exact,
        templates,
        approx_by_edo: Mutex::new(HashMap::new()),
        embed12_by_edo: Mutex::new(HashMap::new()),
    }
}

/// Lookup chord names for `pitch_classes` (already in `[0, edo)`) at the
/// given EDO. Each `rootPc` is tried as the chord root in turn; only roots
/// with an exact / embedded / approximate match get a non-empty `names`
/// list, but all are returned so the caller can rank them with
/// [`name_score`] / [`best_name`] / `rootResultScore` (see `assets/live.js`).
pub fn find_chord_names(db: &ScalaChordDb, edo: i32, pitch_classes: &[i32]) -> Vec<ChordResult> {
    if edo <= 0 {
        return Vec::new();
    }
    db.ensure_for_edo(edo);

    let approx = db.approx_by_edo.lock().unwrap();
    let embed = db.embed12_by_edo.lock().unwrap();
    let patterns_exact = db.by_edo_exact.get(&edo);
    let patterns_embed = embed.get(&edo);
    let patterns_approx = approx.get(&edo);

    let pcs: Vec<i32> = pitch_classes
        .iter()
        .map(|pc| norm_pc(edo, *pc))
        .collect();
    let uniq = uniq_sorted(pcs);
    if uniq.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(uniq.len());
    for &root_pc in &uniq {
        let (rel, pattern) = step_pattern_from_pcs(edo, root_pc, &uniq);
        let mut names: Vec<String> = Vec::new();
        if !pattern.is_empty() {
            if let Some(map) = patterns_exact {
                if let Some(ns) = map.get(&pattern) {
                    for n in ns {
                        names.push(n.clone());
                    }
                }
            }
            if edo != 12 {
                if let Some(map) = patterns_embed {
                    if let Some(ns) = map.get(&pattern) {
                        for n in ns {
                            if !names.iter().any(|x| x == n) {
                                names.push(n.clone());
                            }
                        }
                    }
                }
            }
            if let Some(map) = patterns_approx {
                if let Some(ns) = map.get(&pattern) {
                    for n in ns {
                        if !names.iter().any(|x| x == n) {
                            names.push(n.clone());
                        }
                    }
                }
            }
        }
        out.push(ChordResult {
            root_pc,
            rel,
            pattern,
            names,
        });
    }
    out
}

/// Lower is better. Mirror of `nameScore` in xenwooting's `chords.js` (and
/// duplicated in `assets/live.js`); Rust copy is here so the SSE thread can
/// pre-rank names before emitting.
pub fn name_score(name: &str) -> i32 {
    let s = name;
    let lower = s.to_ascii_lowercase();
    let mut score = 0;
    if lower.contains("-edo12") { score -= 800; }
    if lower.contains("major triad") { score -= 1200; }
    if lower.contains("minor triad") { score -= 1200; }
    if lower.contains("overtone") { score += 500; }
    if lower.contains("undertone") { score += 500; }
    if lower.starts_with("neutral triad") {
        score -= 2200;
    } else if lower.contains("neutral triad") {
        score -= 1600;
    }
    if regex_like_cents_err(&lower) { score += 220; }
    if lower.contains("inversion") { score += 1000; }
    if lower.contains("2nd inversion") { score += 30; }
    if lower.contains("1st inversion") { score += 20; }
    if lower.contains("3rd inversion") { score += 40; }
    if lower.contains("4th inversion") { score += 50; }
    if lower.starts_with("nm ") { score += 350; }
    if lower.contains("split fifth") { score += 180; }
    if lower.contains('|') { score += 90; }
    if lower.contains("quasi-") { score += 80; }
    if lower.contains("ultra-gothic") { score += 120; }
    if lower.contains("tredecimal") { score += 80; }
    if lower.contains("trevicesimal") { score += 80; }
    if lower.contains("bivalent") { score += 60; }
    if lower.contains("subfocal") { score += 60; }
    if lower.contains("isoharmonic") { score += 60; }
    if lower.contains("neo-medieval") { score += 100; }

    score += (s.len() as i32 * 2).min(500);
    if s.len() > 22 {
        score += (((s.len() as i32) - 22) * 6).min(800);
    }

    let busy: i32 = s.chars().filter(|c| matches!(c, '(' | ')' | '"' | '\'')).count() as i32;
    score += busy * 10;
    let comma_count = s.chars().filter(|c| *c == ',').count() as i32;
    score += comma_count * 40;
    if comma_count >= 2 { score += 120; }
    score
}

/// Cheap stand-in for the JS `~\d+c\b` regex test on the lowercase name.
fn regex_like_cents_err(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'~' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'c' {
                let after = if j + 1 < bytes.len() { bytes[j + 1] } else { b' ' };
                let word = after.is_ascii_alphanumeric() || after == b'_';
                if !word {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

pub fn best_name<'a>(names: &'a [String]) -> &'a str {
    if names.is_empty() {
        return "";
    }
    let mut idx_score: Vec<(usize, i32)> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.is_empty())
        .map(|(i, n)| (i, name_score(n)))
        .collect();
    idx_score.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| names[a.0].cmp(&names[b.0]))
    });
    idx_score
        .first()
        .map(|(i, _)| names[*i].as_str())
        .unwrap_or("")
}

// ---------- module-global DB ----------

static CHORDNAM_DB: OnceLock<Arc<ScalaChordDb>> = OnceLock::new();

/// Returns a process-wide chord DB initialised on first access. Parsing
/// `chordnam.par` (~30 KB) takes a few ms; subsequent calls are O(1).
pub fn db() -> &'static Arc<ScalaChordDb> {
    CHORDNAM_DB.get_or_init(|| Arc::new(parse_chordnam(CHORDNAM_PAR)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_step_pattern_marker() {
        assert!(is_step_pattern_line("4-3-3 Major Triad 1st inversion"));
        assert!(is_step_pattern_line("2-2-2-1 Some Tetrachord"));
        assert!(!is_step_pattern_line("4:5:6 Major Triad"));
        assert!(!is_step_pattern_line("Just words"));
        assert!(!is_step_pattern_line("4 Without dashes"));
    }

    #[test]
    fn parses_split_two_spaces() {
        let (a, b) = split_two_spaces("foo  bar baz").unwrap();
        assert_eq!(a, "foo");
        assert_eq!(b, "bar baz");
        assert!(split_two_spaces("foo bar").is_none());
    }

    #[test]
    fn major_triad_name_in_12_edo_via_ratios() {
        // `4:5:6  Fourth-Sixth Chord, Major Triad 2nd inversion` is in the
        // file as a ratio template that projects to 12-EDO as steps 0/4/7
        // and other rotations. Lookup with PCs {0, 4, 7} should produce a
        // name list containing "Major Triad" (or similar) on at least one
        // root pc (rotation/inversion).
        let db = db();
        let results = find_chord_names(db, 12, &[0, 4, 7]);
        let any_major = results.iter().any(|r| {
            r.names
                .iter()
                .any(|n| n.to_lowercase().contains("major triad"))
        });
        assert!(
            any_major,
            "expected a Major Triad name across roots, got {results:?}",
        );
    }

    #[test]
    fn name_score_prefers_shorter_canonical_names() {
        let canonical = name_score("Major Triad");
        let busy = name_score("Some Very Verbose, Many-Comma, (Parenthesised) Variant");
        assert!(canonical < busy, "canonical={canonical} busy={busy}");
    }

    #[test]
    fn best_name_picks_lowest_score() {
        let names = vec![
            "Some Verbose Variant Name (with parentheses)".to_string(),
            "Major Triad".to_string(),
        ];
        assert_eq!(best_name(&names), "Major Triad");
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let db = db();
        assert!(find_chord_names(db, 31, &[]).is_empty());
    }

    #[test]
    fn db_loads_at_least_some_edo12_patterns() {
        let db = db();
        let edo12 = db.by_edo_exact.get(&12).expect("12-EDO block parsed");
        assert!(!edo12.is_empty(), "expected at least some 12-EDO step patterns");
    }

    #[test]
    fn major_triad_in_31_edo_via_template_projection() {
        // 31-EDO has no <SCALA_SCALE_DEF> block in chordnam.par, so all
        // names must come from `approx_by_edo` (template projection).
        // 4:5:6 projects to step pattern 10-8 with ≤5 c error per pitch.
        let db = db();
        let res = find_chord_names(db, 31, &[0, 10, 18]);
        let any_major = res.iter().any(|r| {
            r.names.iter().any(|n| n.to_lowercase().contains("major triad"))
        });
        assert!(any_major, "expected a Major Triad name; got {res:#?}");
    }

    #[test]
    fn dominant_seventh_in_31_edo() {
        // 4:5:6:7 (harmonic seventh) in 31-EDO projects roughly to
        // 0/10/18/25 (depending on rounding). Just sanity-check that we
        // find SOME name list non-empty.
        let db = db();
        let res = find_chord_names(db, 31, &[0, 10, 18, 25]);
        let total_named: usize = res.iter().map(|r| r.names.len()).sum();
        assert!(total_named > 0, "expected at least one name; got {res:#?}");
    }

    #[test]
    fn approx_table_populated_for_31_edo() {
        let db = db();
        db.ensure_for_edo(31);
        let approx = db.approx_by_edo.lock().unwrap();
        let m = approx.get(&31).expect("approx for 31");
        assert!(
            !m.is_empty(),
            "expected approx_by_edo[31] to contain entries"
        );
    }

    /// Run with `cargo test dump_31_edo -- --nocapture`. Not really an
    /// assertion test; prints sample approx patterns for 31-EDO so we can
    /// eyeball what name lookups produce.
    #[test]
    fn dump_31_edo_approx_patterns() {
        let db = db();
        db.ensure_for_edo(31);
        let approx = db.approx_by_edo.lock().unwrap();
        let m = approx.get(&31).expect("approx 31");
        eprintln!("\n=== 31-EDO approx patterns: {} entries ===", m.len());
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort();
        for k in keys.iter().take(15) {
            eprintln!("  {} → {} name(s):", k, m[*k].len());
            for n in m[*k].iter().take(3) {
                eprintln!("    - {}", n);
            }
        }

        // Also dump what a 4:5:6 chord lookup actually produces.
        eprintln!("\n=== find_chord_names(31, [0, 10, 18]) ===");
        drop(approx);
        let res = find_chord_names(db, 31, &[0, 10, 18]);
        for r in &res {
            eprintln!(
                "  rootPc={} pattern={} names={:?}",
                r.root_pc, r.pattern, r.names
            );
        }

        eprintln!("\n=== find_chord_names(31, [0, 9, 18]) === (alt minor-ish triad)");
        let res = find_chord_names(db, 31, &[0, 9, 18]);
        for r in &res {
            eprintln!(
                "  rootPc={} pattern={} names={:?}",
                r.root_pc, r.pattern, r.names
            );
        }
    }
}
