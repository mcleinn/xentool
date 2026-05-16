// Live HUD frontend (T7) — port of xenwooting's LivePage.tsx.
//
// Subscribes to /api/live/stream, renders the four xenwooting views
// (notes / pcs / delta / intervals) with a black canvas, four corner
// status fields, an auto-fit centered glyph line, and an intervals/chord
// area below.
//
// Note-name glyphs (`unicode`/`short` per pitch) and chord names arrive
// as decorations on the SSE state (T8 adds chord names from chordnam.par
// in the Rust SSE handler; T9 adds note glyphs from xenharm_service when
// reachable). When either is absent the renderer falls back to numeric
// labels — same convention as `formatNoteUnicode || String(p)` in
// LivePage.tsx.

(function () {
  'use strict';

  // ---------- helpers (ported verbatim from LivePage.tsx) ----------

  function mod(n, m) {
    var x = n % m;
    return x < 0 ? x + m : x;
  }

  function uniqSorted(nums) {
    var seen = Object.create(null);
    for (var i = 0; i < nums.length; i++) seen[nums[i]] = true;
    var out = Object.keys(seen).map(function (s) { return parseInt(s, 10); });
    out.sort(function (a, b) { return a - b; });
    return out;
  }

  function englishInterval12(semitones) {
    var s = mod(semitones, 12);
    return [
      'Unison', 'Minor 2', 'Major 2', 'Minor 3', 'Major 3', 'Perfect 4',
      'Tritone', 'Perfect 5', 'Minor 6', 'Major 6', 'Minor 7', 'Major 7',
    ][s] || '';
  }

  function nameScore(name) {
    var s = String(name || '');
    var lower = s.toLowerCase();
    var score = 0;
    if (lower.indexOf('-edo12') !== -1) score -= 800;
    if (lower.indexOf('major triad') !== -1) score -= 1200;
    if (lower.indexOf('minor triad') !== -1) score -= 1200;
    if (lower.indexOf('overtone') !== -1) score += 500;
    if (lower.indexOf('undertone') !== -1) score += 500;
    if (lower.indexOf('neutral triad') === 0) score -= 2200;
    else if (lower.indexOf('neutral triad') !== -1) score -= 1600;
    if (/~\d+c\b/.test(lower)) score += 220;
    if (lower.indexOf('inversion') !== -1) score += 1000;
    if (lower.indexOf('2nd inversion') !== -1) score += 30;
    if (lower.indexOf('1st inversion') !== -1) score += 20;
    if (lower.indexOf('3rd inversion') !== -1) score += 40;
    if (lower.indexOf('4th inversion') !== -1) score += 50;
    if (lower.indexOf('nm ') === 0) score += 350;
    if (lower.indexOf('split fifth') !== -1) score += 180;
    if (lower.indexOf('|') !== -1) score += 90;
    if (lower.indexOf('quasi-') !== -1) score += 80;
    if (lower.indexOf('ultra-gothic') !== -1) score += 120;
    if (lower.indexOf('tredecimal') !== -1) score += 80;
    if (lower.indexOf('trevicesimal') !== -1) score += 80;
    if (lower.indexOf('bivalent') !== -1) score += 60;
    if (lower.indexOf('subfocal') !== -1) score += 60;
    if (lower.indexOf('isoharmonic') !== -1) score += 60;
    if (lower.indexOf('neo-medieval') !== -1) score += 100;
    score += Math.min(500, s.length * 2);
    if (s.length > 22) score += Math.min(800, (s.length - 22) * 6);
    var paren = s.match(/[()"']/g);
    if (paren) score += paren.length * 10;
    var commas = s.match(/,/g);
    if (commas) {
      score += commas.length * 40;
      if (commas.length >= 2) score += 120;
    }
    return score;
  }

  function bestName(names) {
    if (!Array.isArray(names) || names.length === 0) return '';
    var sorted = names.slice().filter(Boolean).sort(function (a, b) {
      return nameScore(a) - nameScore(b) || String(a).localeCompare(String(b));
    });
    return sorted[0] || '';
  }

  function rootResultScore(r) {
    var n = Array.isArray(r.names) ? r.names : [];
    var hasNames = n.length > 0;
    var bn = bestName(n);
    var bnLower = bn.toLowerCase();
    var isInversion = bnLower.indexOf('inversion') !== -1;
    var tones = Array.isArray(r.rel) ? r.rel.length : 0;
    return (hasNames ? 0 : 10000) + (isInversion ? 1000 : 0) + tones * 10 + (bn ? nameScore(bn) : 0);
  }

  // Build the list of available glyph spellings for a pitch:
  // primary unicode first, then any non-empty alts. Order is stable so
  // a `noteVariantIndex` value persists meaningfully across renders.
  function noteVariants(v) {
    if (!v || !v.unicode) return [];
    var out = [v.unicode];
    if (Array.isArray(v.alts)) {
      for (var i = 0; i < v.alts.length; i++) {
        if (v.alts[i] && v.alts[i].unicode) out.push(v.alts[i].unicode);
      }
    }
    return out;
  }

  function formatNoteUnicode(v, idx) {
    var variants = noteVariants(v);
    if (variants.length === 0) return '';
    var n = variants.length;
    var i = (((idx | 0) % n) + n) % n;
    return variants[i];
  }

  // Fallback label used when xenharm hasn't filled the note cache for a pitch.
  // For 12-EDO, return the standard MIDI letter name + octave (C4=60). For
  // other EDOs, show "o<octave>p<pitch-class>" so the user at least sees the
  // octave-class decomposition rather than a bare absolute-pitch integer.
  var EDO12_NAMES = ['C', 'C#', 'D', 'Eb', 'E', 'F', 'F#', 'G', 'Ab', 'A', 'Bb', 'B'];

  // Octave numbering: 12-EDO uses the standard MIDI convention (C4=60), all
  // other EDOs use floor-division of the absolute pitch by the EDO step
  // count so the octave matches the layout's `(chan-1)*edo + key` encoding.
  function octaveOf(p, edo) {
    return edo === 12 ? Math.floor(p / 12) - 1 : Math.floor(p / edo);
  }

  // HTML form of pc/octave: `pc<sup class="liveOct">octave</sup>`. Used
  // wherever pc/octave appears so the octave reads as a small
  // superscript rather than a `pc/oct` fraction. Callers must assign
  // to innerHTML, not textContent.
  function pcOctHtml(p, edo) {
    return mod(p, edo) + '<sup class="liveOct">' + octaveOf(p, edo) + '</sup>';
  }

  // Plain-text fallback (no DOM). Currently unused by render paths but
  // kept so any non-DOM consumer (debug overlays, etc.) doesn't see
  // `13<sub>...` in their output.
  function pcOctLabel(p, edo) {
    return mod(p, edo) + '/' + octaveOf(p, edo);
  }

  function fallbackLabel(p, edo) {
    if (typeof p !== 'number' || !isFinite(p)) return String(p);
    if (typeof edo !== 'number' || edo <= 0) return String(p);
    if (edo === 12) {
      var pc12 = mod(p, 12);
      var oct12 = octaveOf(p, 12);
      return EDO12_NAMES[pc12] + oct12;
    }
    return 'o' + octaveOf(p, edo) + 'p' + mod(p, edo);
  }
  function pitchLabel(p, edo) {
    var idx = noteVariantIndex[p] || 0;
    return formatNoteUnicode(noteNameFor(edo, p), idx) || fallbackLabel(p, edo);
  }
  // Bare note name without any octave numeral. Used as the building
  // block for pitchLabelHtml, which appends the octave as a superscript.
  // Falling back: 12-EDO returns "C" / "D♯" / etc.; other EDOs without
  // a xenharm glyph return the bare pitch class as a number — a number
  // is unambiguously the pc.
  function bareNoteName(p, edo) {
    var idx = noteVariantIndex[p] || 0;
    var glyph = formatNoteUnicode(noteNameFor(edo, p), idx);
    if (glyph) return glyph;
    if (typeof p !== 'number' || !isFinite(p)) return String(p);
    if (typeof edo !== 'number' || edo <= 0) return String(p);
    if (edo === 12) return EDO12_NAMES[mod(p, 12)];
    return String(mod(p, edo));
  }
  // True iff xenharm has supplied a real glyph for this pitch. The
  // chord/secondary line uses this to decide whether to append the
  // `(pc⁴)` parenthetical: the parens are informative against an
  // unfamiliar glyph but redundant when the bare name is itself the pc
  // (pure number) or a standard 12-EDO letter.
  function hasXenharmGlyph(edo, p) {
    var v = noteNameFor(edo, p);
    return !!(v && v.unicode);
  }
  // HTML form of the note label: bare name + superscript octave.
  // Callers assign to innerHTML, not textContent.
  function pitchLabelHtml(p, edo) {
    return escapeHtml(bareNoteName(p, edo)) +
      '<sup class="liveOct">' + octaveOf(p, edo) + '</sup>';
  }
  // How many distinct spellings are available for this pitch. 0 means
  // we'll fall back to numeric (no glyph, no cycling); 1 means there's
  // only the primary; >=2 means a click cycles through them.
  function variantCountForPitch(edo, p) {
    return noteVariants(noteNameFor(edo, p)).length;
  }
  // Defensive HTML escape — note glyphs from xenharm shouldn't contain
  // `<` / `&`, but treating any string as text-not-markup is safer.
  function escapeHtml(s) {
    if (s == null) return '';
    return String(s)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // fitText alternates writes (`fontSize`) with reads (`scrollWidth` /
  // `scrollHeight`) inside a loop, which forces a full layout reflow on every
  // iteration. Up to ~80 reflows per call → 50–150 ms of synchronous work per
  // render on modest hardware. We cache the last (text, container size, edge
  // budgets) → fitted size, so identical re-renders short-circuit and the
  // SSE pipeline doesn't pile up.
  var fitTextCache = null;
  function fitText(el, container, startPx, minPx) {
    var key = el.innerHTML + '|' + container.clientWidth + 'x' + container.clientHeight + '|' + startPx + ',' + minPx;
    if (fitTextCache && fitTextCache.key === key) {
      el.style.fontSize = fitTextCache.size + 'px';
      return;
    }
    var size = startPx;
    el.style.fontSize = size + 'px';
    var maxW = container.clientWidth * 0.96;
    var maxH = container.clientHeight * 0.60;
    var guard = 200;
    while (guard-- > 0 && size > minPx && (el.scrollWidth > maxW || el.scrollHeight > maxH)) {
      size -= 2;
      el.style.fontSize = size + 'px';
    }
    fitTextCache = { key: key, size: size };
  }

  // ---------- DOM refs ----------

  var rootEl = document.getElementById('liveRoot');
  var mainEl = document.getElementById('liveMain');
  var mainTextEl = document.getElementById('liveMainText');
  var intervalsEl = document.getElementById('liveIntervals');
  var hintEl = document.getElementById('liveHint');
  var cornerTL = document.getElementById('cornerTL');
  var cornerTR = document.getElementById('cornerTR');
  var cornerBL = document.getElementById('cornerBL');
  var cornerBR = document.getElementById('cornerBR');
  var popoverEl = document.getElementById('livePopover');
  var popoverTitle = document.getElementById('popoverTitle');
  var popoverList = document.getElementById('popoverList');
  var oscPanelEl = document.getElementById('oscPanel');
  var oscParamsEl = document.getElementById('oscParams');
  var oscEventsEl = document.getElementById('oscEvents');
  var footerEl = document.getElementById('liveFooter');

  // ---------- state ----------

  var live = null;
  var view = 'notes'; // notes | pcs | delta | intervals
  var lastSeq = -1;
  var popoverTimer = null;
  // abs_pitch → integer index into noteVariants(...) list for the
  // current preferred spelling. Persists across release/press; cleared
  // when the layout changes (pitch numbers may shift meaning across
  // EDO / pitch_offset changes).
  var noteVariantIndex = Object.create(null);
  var lastLayoutId = null;

  function noteNameFor(edo, pitch) {
    if (!live || !live.note_names) return null;
    var v = live.note_names[edo + ':' + pitch];
    return v || null;
  }

  function intervalNameFor(edo, steps) {
    if (!live || !live.interval_names) return null;
    var v = live.interval_names[edo + ':' + steps];
    return v || null;
  }

  // Compact interval label for the chord-line `+N (...)` decoration.
  // Uses xenharm's glyph or short form, or a 12-EDO English name.
  // Returns '' if nothing's available — callers (computeIntervalLines)
  // emit a bare `+N` in that case rather than a cents annotation, which
  // is just visual noise once xenharm is wired up.
  function intervalLabel(steps, edo) {
    var iv = intervalNameFor(edo, steps);
    if (iv && iv.unicode) return iv.unicode;
    if (iv && iv.short) return iv.short;
    if (edo === 12) {
      var n = englishInterval12(steps).replace(/\s+/g, '');
      if (n) return n;
    }
    return '';
  }

  // ---------- view derivation ----------

  function pressedCombined() {
    if (!live || !live.pressed) return [];
    var all = [];
    var keys = Object.keys(live.pressed);
    for (var i = 0; i < keys.length; i++) {
      var arr = live.pressed[keys[i]];
      if (Array.isArray(arr)) {
        for (var j = 0; j < arr.length; j++) all.push(arr[j]);
      }
    }
    return uniqSorted(all);
  }

  function chordResults() {
    return Array.isArray(live && live.chord) ? live.chord : [];
  }

  // Returns an HTML string. Callers assign to innerHTML so subscript
  // octaves and per-note <span> wrappers render correctly.
  function computeMainText(pressed, edo) {
    if (pressed.length === 0) return '';
    if (view === 'pcs') {
      // Collapse to one entry per pitch class, keeping the lowest octave
      // each pc was pressed in. Output as "pc<sup>octave</sup>" joined
      // with dashes, rendered visually like `14³-21³-3⁴`.
      var byPc = Object.create(null);
      for (var i = 0; i < pressed.length; i++) {
        var p = pressed[i];
        var pc = mod(p, edo);
        if (byPc[pc] === undefined || p < byPc[pc]) byPc[pc] = p;
      }
      var pcKeys = Object.keys(byPc)
        .map(function (s) { return parseInt(s, 10); })
        .sort(function (a, b) { return a - b; });
      return pcKeys.map(function (pc) {
        return pcOctHtml(byPc[pc], edo);
      }).join('-');
    }
    if (view === 'delta') {
      var rootPitch = pressed[0];
      // pitchLabelHtml emits HTML (bare name + superscript octave).
      var rootHtml = pitchLabelHtml(rootPitch, edo);
      var deltas = uniqSorted(pressed.map(function (p) { return mod(p - rootPitch, edo); }))
        .filter(function (d) { return d !== 0; });
      var rootDisplay = hasXenharmGlyph(edo, rootPitch)
        ? (rootHtml + ' (' + pcOctHtml(rootPitch, edo) + ')')
        : rootHtml;
      var parts = [rootDisplay].concat(
        deltas.map(function (d) { return '+' + d; })
      );
      return parts.join(' ');
    }
    // 'notes' view: each pitch becomes its own clickable span so the
    // user can tap to cycle that note's enharmonic spelling. The span
    // contains pre-built HTML (note glyph + superscript octave).
    return pressed.map(function (p) {
      var inner = pitchLabelHtml(p, edo);
      var hasAlts = variantCountForPitch(edo, p) > 1;
      var cls = 'liveNote' + (hasAlts ? ' liveNoteAlts' : '');
      return '<span class="' + cls + '" data-pitch="' + p + '">' +
        inner + '</span>';
    }).join(' ');
  }

  function computeIntervalLines(pressed, edo) {
    var pitchClasses = uniqSorted(pressed.map(function (p) { return mod(p, edo); }));
    if (pitchClasses.length < 2) return [];
    var wantAllRoots = view === 'intervals';

    // Lowest pressed pitch per pitch-class — used to derive a root note name.
    var rootPitchByPc = Object.create(null);
    for (var i = 0; i < pressed.length; i++) {
      var p = pressed[i];
      var pc = mod(p, edo);
      if (rootPitchByPc[pc] === undefined || p < rootPitchByPc[pc]) rootPitchByPc[pc] = p;
    }

    var rootsRaw = chordResults();
    if (rootsRaw.length === 0) {
      rootsRaw = pitchClasses.map(function (rootPc) {
        return { rootPc: rootPc, rel: [], pattern: '', names: [] };
      });
    }

    var rootsSorted = rootsRaw.slice().sort(function (a, b) {
      var sa = rootResultScore(a);
      var sb = rootResultScore(b);
      if (sa !== sb) return sa - sb;
      return a.rootPc - b.rootPc;
    });
    var withNames = rootsSorted.filter(function (r) {
      return Array.isArray(r.names) && r.names.length > 0;
    });
    var roots = wantAllRoots
      ? (withNames.length ? withNames : rootsSorted.slice(0, 1))
      : rootsSorted.slice(0, 1);

    var out = [];
    for (var k = 0; k < roots.length; k++) {
      var r = roots[k];
      var rootPitch = rootPitchByPc[r.rootPc];
      // HTML — bare note name + superscript octave when we have a real
      // pressed pitch; plain pc number otherwise. Either form is safe to
      // drop straight into innerHTML downstream.
      var rootName = rootPitch !== undefined
        ? pitchLabelHtml(rootPitch, edo)
        : String(r.rootPc);
      // "pc/octave" tag used inside the chord-line parens. Only carried
      // through when `showPcOct` is true — i.e. when the bare note name is
      // a real xenharm glyph and the parenthetical pc/oct adds info.
      var rootPcOct = rootPitch !== undefined
        ? pcOctHtml(rootPitch, edo)
        : String(r.rootPc);
      var showPcOct = rootPitch !== undefined && hasXenharmGlyph(edo, rootPitch);
      var rel = (Array.isArray(r.rel) && r.rel.length)
        ? r.rel
        : pitchClasses.map(function (pc) { return mod(pc - r.rootPc, edo); }).sort(function (a, b) { return a - b; });
      var deltas = rel.filter(function (d) { return d !== 0; });
      var deltaText = deltas.map(function (d) {
        var label = intervalLabel(d, edo);
        return label ? ('+' + d + '(' + label + ')') : ('+' + d);
      }).join('');
      var allNames = Array.isArray(r.names)
        ? r.names.slice().filter(Boolean).sort(function (a, b) {
            return nameScore(a) - nameScore(b) || String(a).localeCompare(String(b));
          })
        : [];
      var best = bestName(allNames);
      out.push({
        rootPc: r.rootPc,
        rootPcOct: rootPcOct,
        rootName: rootName,
        showPcOct: showPcOct,
        pattern: r.pattern || '',
        deltaText: deltaText,
        bestName: best,
        allNames: allNames,
      });
    }
    return out;
  }

  // ---------- popover ----------

  function closePopover() {
    if (popoverTimer !== null) {
      clearTimeout(popoverTimer);
      popoverTimer = null;
    }
    popoverEl.hidden = true;
  }

  // `title` is an HTML string (callers concatenate `<sub>` subscripts
  // for octave numerals into it). Names are still plain text.
  function openPopover(title, names) {
    if (popoverTimer !== null) {
      clearTimeout(popoverTimer);
      popoverTimer = null;
    }
    popoverTitle.innerHTML = title;
    popoverList.innerHTML = '';
    for (var i = 0; i < names.length; i++) {
      var row = document.createElement('div');
      row.className = 'livePopoverRow';
      row.textContent = names[i];
      popoverList.appendChild(row);
    }
    popoverEl.hidden = false;
    popoverTimer = setTimeout(function () {
      popoverTimer = null;
      popoverEl.hidden = true;
    }, 4000);
  }

  // ---------- render ----------

  function render() {
    if (!live) return;
    var layout = live.layout || {};
    var mode = live.mode || {};
    var edo = (typeof layout.edo === 'number' && layout.edo > 0) ? layout.edo : 12;
    var pitchOffset = (typeof layout.pitch_offset === 'number') ? layout.pitch_offset : 0;
    var layoutName = layout.name || layout.id || 'Live';

    var pressed = pressedCombined();
    var mainText = computeMainText(pressed, edo);
    // Assign as HTML so per-note <span> wrappers and <sub> octave
    // numerals render correctly. computeMainText escapes its plain-text
    // pieces internally.
    mainTextEl.innerHTML = mainText || '&nbsp;';
    fitText(mainTextEl, mainEl, 180, 22);

    cornerTL.textContent = layoutName;
    cornerTR.textContent = 'edo ' + edo + (pitchOffset ? (' off ' + pitchOffset) : '');
    cornerBL.textContent = (typeof mode.press_threshold === 'number')
      ? ('thr ' + mode.press_threshold.toFixed(2))
      : '';
    var brBits = [];
    if (mode.aftertouch) {
      var atStr = 'at ' + mode.aftertouch;
      if (mode.aftertouch !== 'off' && typeof mode.aftertouch_speed_max === 'number') {
        atStr += ' sp ' + mode.aftertouch_speed_max.toFixed(1);
      }
      brBits.push(atStr);
    }
    if (mode.velocity_profile) brBits.push('vel ' + mode.velocity_profile);
    if (typeof mode.octave_shift === 'number') brBits.push('oct ' + mode.octave_shift);
    if (mode.backend) brBits.push(mode.backend);
    cornerBR.textContent = brBits.join(' ');

    intervalsEl.innerHTML = '';
    var lines = computeIntervalLines(pressed, edo);
    for (var i = 0; i < lines.length; i++) {
      var it = lines[i];
      // `rootName` and `rootPcOct` are HTML fragments (note name with
      // a superscript octave); `deltaText` is plain text and needs
      // escaping. Assign with innerHTML below. Parens only shown when
      // a xenharm glyph is in play — otherwise the bare name already
      // *is* the pc number, so the parenthetical is redundant noise.
      var title = it.showPcOct
        ? (it.rootName + '(' + it.rootPcOct + ')')
        : it.rootName;
      var deltaHtml = escapeHtml(it.deltaText);
      var moreCount = Math.max(0, it.allNames.length - (it.bestName ? 1 : 0));
      var showMore = view !== 'intervals' && moreCount > 0;

      if (view === 'intervals') {
        var block = document.createElement('div');
        block.className = 'liveIntervalsLine liveChordBlock';
        var header = document.createElement('div');
        header.className = 'liveChordHeader';
        header.innerHTML = title + deltaHtml;
        block.appendChild(header);
        for (var j = 0; j < it.allNames.length; j++) {
          var n = document.createElement('div');
          n.className = 'liveChordName';
          n.textContent = it.allNames[j];
          block.appendChild(n);
        }
        intervalsEl.appendChild(block);
      } else {
        var line = document.createElement('div');
        line.className = 'liveIntervalsLine liveChordLine';
        var primary = document.createElement('div');
        primary.className = 'liveChordPrimary';
        // Empty when no chord-DB match — the secondary line below shows
        // delta steps with xenharm interval names (when available) which
        // is always informative on its own. No cryptic placeholder.
        primary.textContent = it.bestName || '';
        if (showMore) {
          var btn = document.createElement('button');
          btn.type = 'button';
          btn.className = 'liveMoreBtn';
          btn.textContent = '(+' + moreCount + ')';
          (function (theTitleHtml, theNames) {
            btn.addEventListener('pointerup', function (ev) {
              ev.preventDefault();
              ev.stopPropagation();
              openPopover(theTitleHtml + ' alternatives', theNames);
            });
          })(title, it.allNames.slice());
          primary.appendChild(document.createTextNode(' '));
          primary.appendChild(btn);
        }
        var secondary = document.createElement('div');
        secondary.className = 'liveChordSecondary';
        secondary.innerHTML = title + deltaHtml;
        line.appendChild(primary);
        line.appendChild(secondary);
        intervalsEl.appendChild(line);
      }
    }

    hintEl.textContent = 'tap to change view: ' + view;
    renderOsc(live.osc || {});
    renderFooter(live.xenharm || {});
  }

  function renderFooter(xenharm) {
    var msg = '';
    if (xenharm && xenharm.last_error) {
      msg = 'xenharm: ' + xenharm.last_error;
    }
    footerEl.hidden = msg.length === 0;
    footerEl.textContent = msg;
  }

  function formatOscValue(p) {
    var v = p.value;
    if (typeof v !== 'number' || !isFinite(v)) return String(v);
    var abs = Math.abs(v);
    var rendered;
    if (abs >= 100 || abs === 0) rendered = v.toFixed(0);
    else if (abs >= 10) rendered = v.toFixed(1);
    else if (abs >= 1) rendered = v.toFixed(2);
    else rendered = v.toFixed(3);
    return p.unit ? (rendered + ' ' + p.unit) : rendered;
  }

  function renderOsc(osc) {
    var params = (osc && osc.params) ? osc.params : {};
    var events = (osc && osc.events) ? osc.events : [];
    var paramKeys = Object.keys(params);
    var hasContent = paramKeys.length > 0 || events.length > 0;
    oscPanelEl.hidden = !hasContent;
    if (!hasContent) {
      oscParamsEl.innerHTML = '';
      oscEventsEl.innerHTML = '';
      return;
    }

    // Group params by .group, sort alphabetically within each group.
    var byGroup = Object.create(null);
    paramKeys.forEach(function (k) {
      var p = params[k];
      var g = p.group || '';
      if (!byGroup[g]) byGroup[g] = [];
      byGroup[g].push(p);
    });
    var groupNames = Object.keys(byGroup).sort();
    oscParamsEl.innerHTML = '';
    groupNames.forEach(function (g) {
      if (g) {
        var label = document.createElement('div');
        label.className = 'oscParamGroup';
        label.textContent = g;
        oscParamsEl.appendChild(label);
      }
      byGroup[g].sort(function (a, b) { return a.name.localeCompare(b.name); });
      byGroup[g].forEach(function (p) {
        var row = document.createElement('div');
        row.className = 'oscParamRow';
        var n = document.createElement('span');
        n.className = 'oscParamName';
        n.textContent = p.name;
        var v = document.createElement('span');
        v.className = 'oscParamValue';
        v.textContent = formatOscValue(p);
        row.appendChild(n);
        row.appendChild(v);
        oscParamsEl.appendChild(row);
      });
    });

    oscEventsEl.innerHTML = '';
    events.slice(0, 6).forEach(function (ev) {
      var row = document.createElement('div');
      row.className = 'oscEventRow';
      row.textContent = ev.text;
      oscEventsEl.appendChild(row);
    });
  }

  // ---------- input ----------

  // Per-note tap: cycle the enharmonic spelling for that pitch only.
  // Bound on mainTextEl with delegation to .liveNote spans. Stops the
  // event so it doesn't bubble to rootEl's view-cycle handler.
  mainTextEl.addEventListener('pointerup', function (ev) {
    var t = ev.target && ev.target.closest && ev.target.closest('.liveNote');
    if (!t) return; // background click — let it bubble to rootEl.
    ev.stopPropagation();
    ev.preventDefault();
    var p = parseInt(t.getAttribute('data-pitch'), 10);
    if (!isFinite(p)) return;
    var edo = (live && live.layout && live.layout.edo) | 0;
    var n = variantCountForPitch(edo, p);
    if (n <= 1) return; // single spelling, nothing to cycle.
    noteVariantIndex[p] = ((noteVariantIndex[p] || 0) + 1) % n;
    render();
  });

  rootEl.addEventListener('pointerup', function (ev) {
    var t = ev.target;
    if (t && t.closest && (
      t.closest('.liveIntervals') ||
      t.closest('.livePopover') ||
      t.closest('.liveNote')
    )) return;
    if (!popoverEl.hidden) {
      closePopover();
      return;
    }
    ev.preventDefault();
    view = view === 'notes' ? 'pcs'
      : view === 'pcs' ? 'delta'
      : view === 'delta' ? 'intervals'
      : 'notes';
    render();
  });

  popoverEl.addEventListener('pointerup', function (ev) {
    ev.preventDefault();
    ev.stopPropagation();
    closePopover();
  });

  window.addEventListener('resize', function () {
    fitText(mainTextEl, mainEl, 180, 22);
  });

  // ---------- SSE ----------

  // Set window.LIVE_DEBUG = true in DevTools to log every SSE frame *and*
  // overlay the chord/SSE state on the page (top-left corner). Persists
  // across page reloads via localStorage so you don't have to retype it.
  if (localStorage.getItem('live_debug') === '1') window.LIVE_DEBUG = true;
  var debugOverlayEl = null;
  function ensureDebugOverlay() {
    if (debugOverlayEl) return debugOverlayEl;
    debugOverlayEl = document.createElement('div');
    debugOverlayEl.id = 'liveDebugOverlay';
    debugOverlayEl.style.cssText = [
      'position:absolute', 'top:8px', 'left:8px',
      'max-width:46vw', 'max-height:60vh',
      'overflow:auto',
      'padding:8px 10px',
      'font:11px ui-monospace,Menlo,Consolas,monospace',
      'color:#9be',
      'background:rgba(0,0,0,0.78)',
      'border:1px solid rgba(155,200,238,0.35)',
      'border-radius:8px',
      'white-space:pre-wrap',
      'z-index:50',
      'pointer-events:none',
    ].join(';');
    document.getElementById('liveRoot').appendChild(debugOverlayEl);
    return debugOverlayEl;
  }
  function debugLog(obj) {
    if (!window.LIVE_DEBUG) {
      if (debugOverlayEl) { debugOverlayEl.remove(); debugOverlayEl = null; }
      return;
    }
    var pressedSummary = obj && obj.pressed
      ? Object.keys(obj.pressed).map(function (k) {
          return k + '=[' + (obj.pressed[k] || []).join(',') + ']';
        }).join(' ')
      : '(none)';
    // eslint-disable-next-line no-console
    console.log('[live] seq=' + obj.seq + ' pressed: ' + pressedSummary);
    var overlay = ensureDebugOverlay();
    var chordSummary = Array.isArray(obj.chord)
      ? obj.chord.map(function (r) {
          var ns = (r.names || []).join(' | ') || '<no name>';
          return '  rootPc=' + r.rootPc + ' pat=' + r.pattern + ' → ' + ns;
        }).join('\n')
      : '<missing chord field>';
    overlay.textContent =
      'seq=' + obj.seq + '  edo=' + ((obj.layout && obj.layout.edo) || '?') + '\n' +
      'pressed: ' + pressedSummary + '\n' +
      'chord (' + (Array.isArray(obj.chord) ? obj.chord.length : 0) + '):\n' +
      chordSummary + '\n' +
      'note_names cached: ' + (obj.note_names ? Object.keys(obj.note_names).length : 0);
  }

  // SSE events arrive at the publisher rate (~25 Hz) but `render()` is
  // expensive enough that processing each one synchronously starves the JS
  // event loop on fast play, producing the "stuck note" + multi-second lag
  // symptom. We coalesce: every incoming event updates `pendingState`, and
  // we schedule a single requestAnimationFrame to render whatever's latest
  // when the frame fires. Multiple events between frames collapse into one
  // render that always uses the freshest snapshot — so a release that
  // happened mid-queue can never be visually "stuck" by an older event
  // sitting in the queue behind it.
  var pendingState = null;
  var renderScheduled = false;
  function scheduleRender() {
    if (renderScheduled) return;
    renderScheduled = true;
    requestAnimationFrame(function () {
      renderScheduled = false;
      var obj = pendingState;
      pendingState = null;
      if (!obj) return;
      live = obj;
      render();
    });
  }

  var es = new EventSource('/api/live/stream');
  es.addEventListener('state', function (ev) {
    try {
      var obj = JSON.parse(ev.data || 'null');
      if (!obj || typeof obj !== 'object') return;
      if (typeof obj.seq === 'number' && obj.seq === lastSeq) return;
      if (typeof obj.seq === 'number') lastSeq = obj.seq;
      // Reset per-note spelling preferences when the layout changes —
      // pitch numbers may shift meaning across EDO / pitch_offset
      // changes, so a stored variant index is no longer trustworthy.
      var newLayoutId = (obj.layout && obj.layout.id) || null;
      if (newLayoutId !== lastLayoutId) {
        noteVariantIndex = Object.create(null);
        lastLayoutId = newLayoutId;
      }
      pendingState = obj;
      debugLog(obj);
      scheduleRender();
    } catch (_) {
      // ignore malformed events
    }
  });
  es.onerror = function () {
    // EventSource auto-retries.
  };
})();
