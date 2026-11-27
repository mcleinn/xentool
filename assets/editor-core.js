// Xentool Layout Editor — arithmetic on a keyboard description and a layout.
//
// No DOM in here: the page (editor.js) and the tests (scripts/editor_core_check.js, run by
// `cargo test` under Node) use the same functions.
//
// `geometry` is the JSON of GET /api/geometry: boards → keys with a position (x, y) in one
// hex lattice for all boards, plus `directions` (lattice offsets of the neighbours as the
// player sees them). `layout` is the JSON of GET /api/layout: { edo, pitch_offset,
// boards: { board0: { <key>: { key, chan, color } } } }. Nothing in here knows a keyboard.
//
// Pitch of a key: (chan - 1) * edo + key + pitch_offset. chan 0: the key is off.

(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.XenCore = api;
})(typeof self !== 'undefined' ? self : this, function () {
  'use strict';

  const OFF = Object.freeze({ key: 0, chan: 0, color: '000000' });

  function mod(n, m) { return ((n % m) + m) % m; }

  // Every key of the keyboard: { board, index, key, x, y, outline? }.
  function keyfield(geometry) {
    const cells = [];
    for (const b of geometry.boards) {
      for (const k of b.keys) {
        cells.push({ board: b.name, index: b.index, key: k.key, x: k.x, y: k.y, outline: k.outline || null });
      }
    }
    return cells;
  }

  function id(cell) { return `${cell.board}:${cell.key}`; }

  // "x,y" → key at that lattice position.
  function positionIndex(geometry) {
    const map = new Map();
    for (const c of keyfield(geometry)) map.set(`${c.x},${c.y}`, c);
    return map;
  }

  function entryOf(layout, board, key) {
    return (layout.boards[board] || {})[key];
  }

  // A key is missing when it has no entry or is off.
  function isMissing(entry) {
    return !entry || entry.chan === 0;
  }

  function pitchOf(entry, edo, offset) {
    return (entry.chan - 1) * edo + entry.key + (offset || 0);
  }

  // Step of the tuning within the octave, counted from Key 0 of a channel.
  function pitchClass(pitch, edo, offset) {
    return mod(pitch - (offset || 0), edo);
  }

  // The two lattice directions the generators run along: one key to the right, one key
  // up and to the right, as the player sees the keyboard.
  function basis(geometry) {
    return { a: geometry.directions.right, b: geometry.directions.upper_right };
  }

  // (dx, dy) = dq * a + dr * b
  function basisCoeffs(dx, dy, a, b) {
    const det = a[0] * b[1] - a[1] * b[0];
    return { dq: (dx * b[1] - dy * b[0]) / det, dr: (a[0] * dy - a[1] * dx) / det };
  }

  function predictedPitch(cell, gen, bas) {
    const { dq, dr } = basisCoeffs(cell.x - gen.base.x, cell.y - gen.base.y, bas.a, bas.b);
    return gen.basePitch + dq * gen.gA + dr * gen.gB;
  }

  // Hex rotation by steps of 60° around the lattice origin.
  function rotateHex(x, y, steps) {
    let q = Math.trunc((y - x) / 2);
    let r = x;
    let s = -q - r;
    const k = mod(steps, 6);
    for (let i = 0; i < k; i++) {
      const nq = -r, nr = -s, ns = -q;
      q = nq; r = nr; s = ns;
    }
    return { x: r, y: 2 * q + r };
  }

  // Keys that are set: "x,y" → { cell, entry }.
  function setKeys(geometry, layout) {
    const map = new Map();
    for (const c of keyfield(geometry)) {
      const e = entryOf(layout, c.board, c.key);
      if (!isMissing(e)) map.set(`${c.x},${c.y}`, { cell: c, entry: e });
    }
    return map;
  }

  function mostCommon(counts) {
    let best = null, bestCount = 0;
    for (const [v, n] of counts) if (n > bestCount) { best = v; bestCount = n; }
    return { best, bestCount };
  }

  // Most common pitch step between a set key and its set neighbour in direction d.
  function modeOfDeltas(keys, d, edo, offset) {
    const counts = new Map();
    let edges = 0;
    for (const { cell, entry } of keys.values()) {
      const to = keys.get(`${cell.x + d[0]},${cell.y + d[1]}`);
      if (!to) continue;
      const dp = pitchOf(to.entry, edo, offset) - pitchOf(entry, edo, offset);
      counts.set(dp, (counts.get(dp) || 0) + 1);
      edges++;
    }
    const { best, bestCount } = mostCommon(counts);
    return { best, bestCount, edges };
  }

  // Colour per pitch class: the colour most keys of that pitch class have. Classes without
  // a set key are null.
  function paletteOf(geometry, layout) {
    const edo = layout.edo;
    const counts = Array.from({ length: edo }, () => new Map());
    for (const { entry } of setKeys(geometry, layout).values()) {
      const pc = pitchClass(pitchOf(entry, edo, layout.pitch_offset), edo, layout.pitch_offset);
      const c = (entry.color || '000000').toUpperCase();
      counts[pc].set(c, (counts[pc].get(c) || 0) + 1);
    }
    return counts.map(m => mostCommon(m).best);
  }

  // Generators that fit the layout best, or { error }.
  function guess(geometry, layout) {
    const edo = layout.edo;
    if (!edo) return { error: 'set Edo first' };
    const keys = setKeys(geometry, layout);
    if (keys.size === 0) return { error: 'no pads set (all empty)' };
    const bas = basis(geometry);
    const a = modeOfDeltas(keys, bas.a, edo, layout.pitch_offset);
    const b = modeOfDeltas(keys, bas.b, edo, layout.pitch_offset);
    if (a.best === null || b.best === null) return { error: 'not enough adjacent set pads to guess' };
    const edges = a.edges + b.edges;
    const first = keys.values().next().value;
    return {
      gA: a.best,
      gB: b.best,
      fitPct: edges ? Math.round(1000 * (a.bestCount + b.bestCount) / edges) / 10 : 100,
      palette: paletteOf(geometry, layout).map(c => c || '000000'),
      firstSet: { board: first.cell.board, key: first.cell.key, entry: first.entry },
    };
  }

  // gen = { gA, gB, base: { board, key }, baseKey, baseChan } → with the base key's
  // position and pitch, or null when the keyboard has no such key.
  function resolveGen(geometry, layout, gen) {
    const cell = keyfield(geometry).find(c => c.board === gen.base.board && c.key === gen.base.key);
    if (!cell || !layout.edo) return null;
    return {
      gA: gen.gA,
      gB: gen.gB,
      base: cell,
      basePitch: (gen.baseChan - 1) * layout.edo + gen.baseKey + (layout.pitch_offset || 0),
    };
  }

  // Which keys do not follow the generators: { totalSet, mismatch: [id], missing: [id] }.
  function verify(geometry, layout, gen) {
    const g = resolveGen(geometry, layout, gen);
    if (!g) return null;
    const bas = basis(geometry);
    const res = { totalSet: 0, mismatch: [], missing: [] };
    for (const c of keyfield(geometry)) {
      const e = entryOf(layout, c.board, c.key);
      if (isMissing(e)) { res.missing.push(id(c)); continue; }
      res.totalSet++;
      if (predictedPitch(c, g, bas) !== pitchOf(e, layout.edo, layout.pitch_offset)) res.mismatch.push(id(c));
    }
    return res;
  }

  // How the layout writes a pitch as Chan and Key. A pitch has several spellings
  // (Chan + 1 equals Key + edo). Where every key has Key < edo, the layout counts octaves by
  // channel and a new key does the same. Otherwise (one channel per board, or one channel
  // for everything) a new key takes the channel most keys of its board have, or the
  // nearest channel on which its Key fits 0..127.
  function channelRule(geometry, layout) {
    const edo = layout.edo;
    let octaveChannels = true;
    const perBoard = new Map();
    const all = new Map();
    for (const { cell, entry } of setKeys(geometry, layout).values()) {
      if (entry.key >= edo) octaveChannels = false;
      if (!perBoard.has(cell.board)) perBoard.set(cell.board, new Map());
      const m = perBoard.get(cell.board);
      m.set(entry.chan, (m.get(entry.chan) || 0) + 1);
      all.set(entry.chan, (all.get(entry.chan) || 0) + 1);
    }
    const overall = mostCommon(all).best;
    return (board) => {
      if (octaveChannels) return null;
      const own = perBoard.has(board) ? mostCommon(perBoard.get(board)).best : null;
      return own !== null ? own : overall;
    };
  }

  // Chan (1..16) and Key (0..127) of a pitch, on `preferChan` if the key fits there, else
  // on the nearest channel where it fits; without a preference on the channel of its
  // octave. null: the pitch is outside what 16 channels hold.
  function pitchToChanKey(pitch, edo, offset, preferChan) {
    const rel = pitch - (offset || 0);
    const want = preferChan || Math.floor(rel / edo) + 1;
    let best = null;
    for (let chan = 1; chan <= 16; chan++) {
      const key = rel - (chan - 1) * edo;
      if (key < 0 || key > 127) continue;
      if (best === null || Math.abs(chan - want) < Math.abs(best.chan - want)) best = { chan, key };
    }
    return best;
  }

  // Set the keys that `filter(cell)` picks to what the generators predict. Colour of a key:
  // `palette[pitch class]` when the palette has one (not black), else the colour most keys
  // of the same pitch class have in the layout, else black.
  // Returns { count, changed: [id], skipped: [id] } (skipped: pitch out of range).
  function applyGenerators(geometry, layout, gen, palette, filter) {
    const g = resolveGen(geometry, layout, gen);
    if (!g) return null;
    const edo = layout.edo;
    const offset = layout.pitch_offset || 0;
    const bas = basis(geometry);
    const inUse = paletteOf(geometry, layout);
    const prefer = channelRule(geometry, layout);
    const res = { count: 0, changed: [], skipped: [] };
    for (const c of keyfield(geometry)) {
      if (filter && !filter(c)) continue;
      const p = predictedPitch(c, g, bas);
      const ck = Number.isInteger(p) ? pitchToChanKey(p, edo, offset, prefer(c.board)) : null;
      if (!ck) { res.skipped.push(id(c)); continue; }
      const pc = pitchClass(p, edo, offset);
      const own = palette && palette[pc] && palette[pc] !== '000000' ? palette[pc] : null;
      const color = own || inUse[pc] || '000000';
      if (!layout.boards[c.board]) layout.boards[c.board] = {};
      layout.boards[c.board][c.key] = { key: ck.key, chan: ck.chan, color };
      res.changed.push(id(c));
      res.count++;
    }
    return res;
  }

  // Fill the keys that are missing or do not follow the generators; leave the others.
  function repair(geometry, layout, gen, palette) {
    const v = verify(geometry, layout, gen);
    if (!v) return null;
    const bad = new Set([...v.mismatch, ...v.missing]);
    return applyGenerators(geometry, layout, gen, palette, c => bad.has(id(c)));
  }

  // Take an import of the same geometry over (POST /api/import, mode "direct"): board n
  // of the file becomes board n, key k stays key k. `onlyBoard` limits it to one board.
  // Returns the lines to tell the user.
  function mergeDirectImport(layout, imp, onlyBoard) {
    const notes = [];
    let keys = 0;
    const boards = [];
    for (const [name, pads] of Object.entries(imp.boards)) {
      if (onlyBoard && name !== onlyBoard) continue;
      if (!layout.boards[name]) layout.boards[name] = {};
      for (const [k, p] of Object.entries(pads)) {
        layout.boards[name][k] = { key: p.key, chan: p.chan, color: p.color };
        keys++;
      }
      boards.push(name);
    }
    notes.push(`imported ${keys} keys one to one (${boards.join(', ') || 'no board'})`);
    if (imp.edo !== null && imp.edo !== undefined) {
      layout.edo = imp.edo;
      notes.push(`Edo ${imp.edo} taken from the import`);
    } else {
      notes.push(`the import has no Edo: kept ${layout.edo ?? 'none'} — set Edo before saving`);
    }
    if (imp.pitch_offset !== null && imp.pitch_offset !== undefined) {
      layout.pitch_offset = imp.pitch_offset;
      notes.push(`PitchOffset ${imp.pitch_offset} taken from the import`);
    } else {
      notes.push(`the import has no PitchOffset: kept ${layout.pitch_offset}`);
    }
    const missing = (imp.missing_boards || []).filter(b => !onlyBoard || b === onlyBoard);
    if (missing.length) notes.push(`not in the import, left as they are: ${missing.join(', ')}`);
    if (imp.disabled_keys) notes.push(`${imp.disabled_keys} disabled keys (KTyp 0 or 4) switched off`);
    if (imp.controller_keys) notes.push(`${imp.controller_keys} controller keys (KTyp 2) taken over as notes`);
    if (imp.dropped && imp.dropped.length) notes.push(`not part of a layout, dropped: ${imp.dropped.join(', ')}`);
    return notes;
  }

  return {
    OFF, mod, keyfield, id, positionIndex, entryOf, isMissing, pitchOf, pitchClass, basis,
    basisCoeffs, predictedPitch, rotateHex, setKeys, paletteOf, guess, resolveGen, verify,
    channelRule, pitchToChanKey, applyGenerators, repair, mergeDirectImport,
  };
});
