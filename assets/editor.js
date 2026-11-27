// Xentool Layout Editor — vanilla JS frontend.
//
// The page draws whatever keyboard GET /api/geometry describes (boards → keys with a place
// in one hex lattice, optionally with a keycap outline) and knows no keyboard by name.
// Geometry and generator arithmetic is in editor-core.js (XenCore).

const HEX_SIZE = 22;
const SHORT = HEX_SIZE * Math.sqrt(3) / 2;
const LONG = HEX_SIZE * 1.5;
const SVG_NS = 'http://www.w3.org/2000/svg';

// Doubled-y lattice: y = horizontal (step 2 between same-row neighbours), x = row.
// The orientation says whether row 0 is at the bottom (YRightXUp) or at the top.
function hexToPixel(x, y) {
  const px = y * SHORT;
  const py = geometry.orientation === 'YRightXUp' ? -x * LONG : x * LONG;
  return { px, py };
}

function hexPolyPoints(cx, cy, r) {
  const pts = [];
  for (let i = 0; i < 6; i++) {
    const angle = Math.PI / 3 * i - Math.PI / 2;
    pts.push(`${cx + r * Math.cos(angle)},${cy + r * Math.sin(angle)}`);
  }
  return pts.join(' ');
}

let geometry = null;
let layout = null;
let cells = [];             // XenCore.keyfield(geometry)
let cellAt = new Map();     // "x,y" → cell
let selection = new Set();
let hoveredPad = null;
let importState = null;  // { pads, tx, ty, rot, scope: 'global'|boardName }
let viewMode = 'combined';  // 'combined' | 'individual'
let displayMode = 'virtual';  // 'virtual' (key/chan) | 'absolute' (single integer pitch)
let dirty = false;
let currentLayoutName = '';

const statusEl = document.getElementById('status');
const boardsEl = document.getElementById('boards');
const layoutSelectEl = document.getElementById('layoutSelect');

function markDirty() {
  if (!dirty) {
    dirty = true;
    document.title = '• ' + document.title.replace(/^• /, '');
  }
}

function clearDirty() {
  if (dirty) {
    dirty = false;
    document.title = document.title.replace(/^• /, '');
  }
}

async function populateLayoutSelect() {
  try {
    const data = await fetch('./api/files').then(r => r.json());
    currentLayoutName = data.current || '';
    layoutSelectEl.innerHTML = '';
    const names = data.files.includes(currentLayoutName) ? data.files : [currentLayoutName, ...data.files];
    for (const name of names) {
      const opt = document.createElement('option');
      opt.value = name;
      opt.textContent = name;
      if (name === currentLayoutName) opt.selected = true;
      layoutSelectEl.appendChild(opt);
    }
  } catch (e) {
    // Leave empty on error; feature degrades gracefully.
  }
}

layoutSelectEl.addEventListener('change', async (e) => {
  const chosen = e.target.value;
  if (chosen === currentLayoutName) return;
  if (dirty) {
    const ok = confirm('You have unsaved changes. Discard and switch?');
    if (!ok) {
      e.target.value = currentLayoutName;
      return;
    }
  }
  const res = await fetch('./api/load', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name: chosen }),
  });
  if (!res.ok) {
    setStatus('load failed: ' + await res.text());
    e.target.value = currentLayoutName;
    return;
  }
  currentLayoutName = chosen;
  clearDirty();
  await loadAll();
  setStatus('switched to ' + chosen);
});

window.addEventListener('beforeunload', (e) => {
  if (dirty) {
    e.preventDefault();
    e.returnValue = '';
  }
});

async function loadAll() {
  geometry = await fetch('./api/geometry').then(r => r.json());
  layout = await fetch('./api/layout').then(r => r.json());
  cells = XenCore.keyfield(geometry);
  cellAt = XenCore.positionIndex(geometry);
  selection.clear();
  mismatchSet.clear();
  missingSet.clear();
  resetImport();
  document.getElementById('edo').value = layout.edo ?? '';
  document.getElementById('pitchOffset').value = layout.pitch_offset;
  const accept = geometry.import_extensions.map(e => '.' + e).join(',');
  document.getElementById('importFile').accept = accept;
  currentLayoutName = geometry.current_file || currentLayoutName;
  clearDirty();
  document.title = `${currentLayoutName} — Xentool Layout Editor`;
  document.getElementById('kindInfo').textContent =
    `${geometry.geometry} · ${geometry.boards.length} board(s) · ${cells.length} keys`;
  const bas = XenCore.basis(geometry);
  document.getElementById('genALabel').textContent = `Gen A (one key right, (${bas.a}))`;
  document.getElementById('genBLabel').textContent = `Gen B (one key up-right, (${bas.b}))`;
  document.getElementById('basePad').max = Math.max(...cells.map(c => c.key));
  showNotice([]);
  await populateLayoutSelect();
  populateBaseBoardOptions();
  renderBoards();
  setStatus('loaded');
}

function setStatus(s) { statusEl.textContent = s; }

// Longer messages (what an import did) stay in the sidebar until the next one.
function showNotice(lines) {
  const el = document.getElementById('notice');
  el.innerHTML = '';
  el.style.display = lines.length ? '' : 'none';
  for (const l of lines) {
    const p = document.createElement('p');
    p.textContent = l;
    el.appendChild(p);
  }
}

function entryOrOff(cell) {
  return XenCore.entryOf(layout, cell.board, cell.key) || XenCore.OFF;
}

// ---- Drawing: one keyfield renderer for every keyboard ----

// Where a key is drawn: its keycap outline if the keyboard gives one, else a hexagon at
// its lattice position.
function shapeOf(cell, outlined) {
  if (outlined) {
    const [x, y, w, h] = cell.outline;
    return { rect: true, x, y, w, h, cx: x + w / 2, cy: y + h / 2, minX: x, maxX: x + w, minY: y, maxY: y + h };
  }
  const { px, py } = hexToPixel(cell.x, cell.y);
  return { rect: false, cx: px, cy: py, minX: px - SHORT, maxX: px + SHORT, minY: py - HEX_SIZE, maxY: py + HEX_SIZE };
}

function outlineEl(shape, ox, oy, inset, cls) {
  let el;
  if (shape.rect) {
    el = document.createElementNS(SVG_NS, 'rect');
    el.setAttribute('x', shape.x + ox + inset);
    el.setAttribute('y', shape.y + oy + inset);
    el.setAttribute('width', shape.w - 2 * inset);
    el.setAttribute('height', shape.h - 2 * inset);
    el.setAttribute('rx', 5);
  } else {
    el = document.createElementNS(SVG_NS, 'polygon');
    el.setAttribute('points', hexPolyPoints(shape.cx + ox, shape.cy + oy, HEX_SIZE - inset));
  }
  el.setAttribute('class', cls);
  return el;
}

function renderBoards() {
  boardsEl.innerHTML = '';
  boardsEl.classList.toggle('vertical-stack', viewMode === 'individual' && cells.some(c => c.outline));
  boardsEl.classList.toggle('combined', viewMode === 'combined');
  if (viewMode === 'combined') {
    boardsEl.appendChild(renderKeyfield(cells, 'All boards (one keyfield)', null));
  } else {
    for (const b of geometry.boards) {
      boardsEl.appendChild(renderKeyfield(cells.filter(c => c.board === b.name), b.name, b.name));
    }
  }
  renderColorsInUse();
}

// Draw `shown` (all keys, or the keys of one board). `board`: name of the single board
// (gets its own Import button), null for the whole keyfield.
function renderKeyfield(shown, title, board) {
  const container = document.createElement('div');
  container.className = 'board-container';
  const h = document.createElement('h3');
  const label = document.createElement('span');
  label.textContent = title;
  h.appendChild(label);
  if (board) {
    const importLbl = document.createElement('label');
    importLbl.className = 'board-header-import';
    importLbl.textContent = 'Import…';
    const importInput = document.createElement('input');
    importInput.type = 'file';
    importInput.accept = document.getElementById('importFile').accept;
    importInput.style.display = 'none';
    importInput.addEventListener('change', (e) => handleImport(e.target.files[0], board));
    importLbl.appendChild(importInput);
    h.appendChild(importLbl);
  }
  container.appendChild(h);

  const outlined = shown.length > 0 && shown.every(c => c.outline);
  const shapes = new Map(shown.map(c => [XenCore.id(c), shapeOf(c, outlined)]));
  let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
  for (const s of shapes.values()) {
    minX = Math.min(minX, s.minX); maxX = Math.max(maxX, s.maxX);
    minY = Math.min(minY, s.minY); maxY = Math.max(maxY, s.maxY);
  }
  const pad = 6;
  const ox = -minX + pad;
  const oy = -minY + pad;
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${(maxX - minX) + 2 * pad} ${(maxY - minY) + 2 * pad}`);
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.setAttribute('class', 'board-svg');

  for (const c of shown) drawKey(svg, c, shapes.get(XenCore.id(c)), ox, oy);

  // Import overlay: the imported keys at the keys they would land on.
  if (importState && (importState.scope === 'global' ? !board : importState.scope === board)) {
    for (const p of importState.pads) {
      const { x, y } = projectImportPad(p);
      const target = cellAt.get(`${x},${y}`);
      const shape = target && shapes.get(XenCore.id(target));
      if (!shape) continue;
      svg.appendChild(outlineEl(shape, ox, oy, 3, 'import-overlay'));
      const lbl = document.createElementNS(SVG_NS, 'text');
      lbl.setAttribute('x', shape.cx + ox);
      lbl.setAttribute('y', shape.cy + oy + HEX_SIZE * 0.75);
      lbl.setAttribute('class', 'import-label');
      lbl.textContent = `${p.key}/${p.chan}`;
      svg.appendChild(lbl);
    }
  }

  container.appendChild(svg);
  return container;
}

function drawKey(svg, cell, shape, ox, oy) {
  const padInfo = entryOrOff(cell);
  const selKey = XenCore.id(cell);
  const cx = shape.cx + ox;
  const cy = shape.cy + oy;

  const g = document.createElementNS(SVG_NS, 'g');
  g.dataset.board = cell.board;
  g.dataset.pad = cell.key;

  const body = outlineEl(shape, ox, oy, shape.rect ? 1 : 0, 'pad');
  body.setAttribute('fill', '#' + padInfo.color);
  if (selection.has(selKey)) body.classList.add('selected');
  if (padInfo.chan === 0) body.classList.add('off');
  g.appendChild(body);

  // Mismatch marker: red dashed border overlay. Missing: grey dashed border.
  if (mismatchSet.has(selKey)) g.appendChild(outlineEl(shape, ox, oy, shape.rect ? 3 : 2, 'pad-mismatch'));
  else if (missingSet.has(selKey)) g.appendChild(outlineEl(shape, ox, oy, shape.rect ? 3 : 2, 'pad-missing'));

  const light = isDark(padInfo.color);
  const lbl = document.createElementNS(SVG_NS, 'text');
  lbl.setAttribute('x', cx);
  lbl.setAttribute('y', cy + 3);
  lbl.setAttribute('class', 'pad-label');
  if (light) lbl.classList.add('light');
  lbl.textContent = padLabel(padInfo);
  g.appendChild(lbl);

  const padIdLbl = document.createElementNS(SVG_NS, 'text');
  padIdLbl.setAttribute('x', cx);
  padIdLbl.setAttribute('y', cy - HEX_SIZE * 0.55);
  padIdLbl.setAttribute('class', 'pad-label');
  if (light) padIdLbl.classList.add('light');
  padIdLbl.setAttribute('style', 'font-size: 8px; opacity: 0.7');
  padIdLbl.textContent = `#${cell.key}`;
  g.appendChild(padIdLbl);

  g.addEventListener('click', (e) => onPadClick(e, cell.board, cell.key));
  // Lattice position of the hovered key: the pivot for turning an import.
  g.addEventListener('mouseenter', () => { hoveredPad = cell; });
  g.addEventListener('mouseleave', () => { if (hoveredPad === cell) hoveredPad = null; });
  svg.appendChild(g);
}

function isDark(hex) {
  const r = parseInt(hex.substring(0, 2), 16);
  const g = parseInt(hex.substring(2, 4), 16);
  const b = parseInt(hex.substring(4, 6), 16);
  const luma = 0.299 * r + 0.587 * g + 0.114 * b;
  return luma < 128;
}

function onPadClick(e, boardName, padId) {
  const key = `${boardName}:${padId}`;
  if (e.shiftKey || e.ctrlKey || e.metaKey) {
    if (selection.has(key)) selection.delete(key);
    else selection.add(key);
  } else {
    selection.clear();
    selection.add(key);
  }
  refreshSelectionPanel();
  renderBoards();
}

function refreshSelectionPanel() {
  const info = document.getElementById('selectionInfo');
  if (selection.size === 0) {
    info.textContent = 'No selection';
    return;
  }
  if (selection.size === 1) {
    const [only] = selection;
    const [bn, pid] = only.split(':');
    const p = (layout.boards[bn] || {})[pid] || XenCore.OFF;
    info.textContent = `${bn} #${pid}: Key=${p.key} Chan=${p.chan}${p.chan === 0 ? ' (off)' : ''} Col=#${p.color}`;
    document.getElementById('edKey').value = p.key;
    document.getElementById('edChan').value = p.chan;
    document.getElementById('edColor').value = '#' + p.color.toLowerCase();
  } else {
    info.textContent = `${selection.size} pads selected`;
  }
}

function ensureBoard(bn) {
  if (!layout.boards[bn]) layout.boards[bn] = {};
  return layout.boards[bn];
}

document.getElementById('apply').addEventListener('click', () => {
  if (selection.size === 0) return setStatus('nothing selected');
  const key = parseInt(document.getElementById('edKey').value, 10);
  const chan = parseInt(document.getElementById('edChan').value, 10);
  const color = document.getElementById('edColor').value.replace('#', '').toUpperCase();
  if (!(key >= 0 && key <= 127) || !(chan >= 0 && chan <= 16)) return setStatus('Key must be 0-127, Chan 0-16');
  for (const sel of selection) {
    const [bn, pidStr] = sel.split(':');
    ensureBoard(bn)[parseInt(pidStr, 10)] = { key, chan, color };
  }
  markDirty();
  setStatus(`applied to ${selection.size} pads`);
  refreshSelectionPanel();
  renderBoards();
});

document.getElementById('enumerate').addEventListener('click', () => {
  const start = parseInt(document.getElementById('enumStart').value, 10);
  const step = parseInt(document.getElementById('enumStep').value, 10);
  const sorted = Array.from(selection).sort((a, b) => {
    const [ba, pa] = a.split(':'); const [bb, pb] = b.split(':');
    if (ba !== bb) return ba.localeCompare(bb);
    return parseInt(pa, 10) - parseInt(pb, 10);
  });
  let k = start;
  for (const sel of sorted) {
    const [bn, pidStr] = sel.split(':');
    const pid = parseInt(pidStr, 10);
    const existing = (layout.boards[bn] || {})[pid] || { chan: 1, color: '000000' };
    ensureBoard(bn)[pid] = { key: Math.max(0, Math.min(127, k)), chan: existing.chan, color: existing.color };
    k += step;
  }
  markDirty();
  setStatus(`enumerated ${sorted.length} pads from ${start} step ${step}`);
  renderBoards();
});

document.getElementById('edo').addEventListener('change', (e) => {
  layout.edo = parseInt(e.target.value, 10) || null;
  markDirty();
  renderBoards();
});
document.getElementById('pitchOffset').addEventListener('change', (e) => {
  layout.pitch_offset = parseInt(e.target.value, 10) || 0;
  markDirty();
  renderBoards();
});

document.getElementById('save').addEventListener('click', async () => {
  const res = await fetch('./api/layout', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(layout),
  });
  if (res.ok) {
    clearDirty();
    setStatus('saved');
  } else {
    setStatus('save failed: ' + await res.text());
  }
});

// View mode toggle (Combined / Individual). Single button that flips state
// on click; the label always shows the *active* mode.
document.getElementById('viewModeToggle').addEventListener('click', (e) => {
  const btn = e.currentTarget;
  viewMode = (btn.dataset.mode === 'combined') ? 'individual' : 'combined';
  btn.dataset.mode = viewMode;
  btn.textContent = viewMode === 'combined' ? 'Combined' : 'Individual';
  // Reset import state on view mode switch to avoid scope mismatch.
  resetImport();
  renderBoards();
  // Global import only makes sense in combined view.
  document.getElementById('globalImportLabel').style.display =
    viewMode === 'combined' ? '' : 'none';
});

// Display mode toggle (Virtual key/chan / Absolute pitch integer). Same pattern.
document.getElementById('displayModeToggle').addEventListener('click', (e) => {
  const btn = e.currentTarget;
  displayMode = (btn.dataset.mode === 'virtual') ? 'absolute' : 'virtual';
  btn.dataset.mode = displayMode;
  btn.textContent = displayMode === 'virtual' ? 'Virtual' : 'Absolute';
  renderBoards();
});

// ---- Import flow ----
//
// The server decides by the geometries (POST /api/import): a file for the same keyboard
// is taken over key by key at once ("direct"); a file for another keyboard comes back as
// keys with lattice positions, to be laid over the open keyboard, moved and turned ("place").

async function handleImport(file, scope) {
  if (!file) return;
  resetImport();
  const text = await file.text();
  const ext = file.name.split('.').pop().toLowerCase();
  const res = await fetch('./api/import', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content: text, kind: ext }),
  });
  if (!res.ok) {
    setStatus('import failed: ' + await res.text());
    return;
  }
  const data = await res.json();
  if (data.mode === 'direct') {
    const notes = XenCore.mergeDirectImport(layout, data, scope === 'global' ? null : scope);
    document.getElementById('edo').value = layout.edo ?? '';
    document.getElementById('pitchOffset').value = layout.pitch_offset;
    populatePresets('genAPreset', 'genA');
    populatePresets('genBPreset', 'genB');
    renderPalette();
    markDirty();
    mismatchSet.clear();
    missingSet.clear();
    showNotice([`Import of ${file.name}:`, ...notes]);
    setStatus(`${notes[0]} — not saved yet`);
    renderBoards();
    return;
  }
  if (data.edo && !layout.edo) {
    layout.edo = data.edo;
    document.getElementById('edo').value = data.edo;
    markDirty();
  }
  importState = { pads: data.pads, tx: 0, ty: 0, rot: 0, scope };
  document.getElementById('applyImport').disabled = false;
  document.getElementById('cancelImport').disabled = false;
  setStatus(`imported ${data.pads.length} source pads (scope: ${scope}) — arrows/R to position, Enter to apply`);
  renderBoards();
}

document.getElementById('importFile').addEventListener('change', (e) => {
  handleImport(e.target.files[0], 'global');
  e.target.value = '';  // allow re-importing the same file
});

function resetImport() {
  importState = null;
  document.getElementById('applyImport').disabled = true;
  document.getElementById('cancelImport').disabled = true;
}

document.getElementById('cancelImport').addEventListener('click', () => {
  resetImport();
  setStatus('import cancelled');
  renderBoards();
});

document.getElementById('applyImport').addEventListener('click', () => {
  if (!importState) return;

  // Every key of the target boards is reset to off, then the imported keys are put on top.
  const inScope = (c) => importState.scope === 'global' || c.board === importState.scope;
  for (const c of cells) {
    if (inScope(c)) ensureBoard(c.board)[c.key] = { key: 0, chan: 0, color: '000000' };
  }
  let applied = 0;
  for (const p of importState.pads) {
    const { x, y } = projectImportPad(p);
    const target = cellAt.get(`${x},${y}`);
    if (!target || !inScope(target)) continue;
    ensureBoard(target.board)[target.key] = { key: p.key, chan: p.chan, color: p.color };
    applied++;
  }
  resetImport();
  markDirty();
  setStatus(`applied ${applied} pads from import; rest reset to 0/0/black`);
  renderBoards();
});

function projectImportPad(p) {
  const rotated = XenCore.rotateHex(p.x, p.y, importState.rot);
  return { x: rotated.x + importState.tx, y: rotated.y + importState.ty };
}

// ---- Keyboard handlers (for import mode) ----

document.addEventListener('keydown', (e) => {
  if (e.target && (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA')) return;
  if (!importState) return;

  // The arrows move the import as the player sees the keyboard, whichever way its rows count.
  const d = geometry.directions;
  const move = (v) => { importState.tx += v[0]; importState.ty += v[1]; renderBoards(); e.preventDefault(); };

  if (e.key === 'ArrowLeft') move(d.left);
  else if (e.key === 'ArrowRight') move(d.right);
  else if (e.key === 'ArrowUp') move(d.upper_right);
  else if (e.key === 'ArrowDown') move(d.lower_left);
  else if (e.key === 'r' || e.key === 'R') {
    const pivot = hoveredPad ? { x: hoveredPad.x, y: hoveredPad.y } : { x: 0, y: 0 };
    rotateImportAround(pivot);
    renderBoards();
    e.preventDefault();
  }
  else if (e.key === 'Enter') {
    document.getElementById('applyImport').click();
    e.preventDefault();
  }
  else if (e.key === 'Escape') {
    document.getElementById('cancelImport').click();
    e.preventDefault();
  }
});

function rotateImportAround(pivot) {
  const pivotMinusT = { x: pivot.x - importState.tx, y: pivot.y - importState.ty };
  const rotated = XenCore.rotateHex(pivotMinusT.x, pivotMinusT.y, 1);
  importState.rot = (importState.rot + 1) % 6;
  importState.tx = pivot.x - rotated.x;
  importState.ty = pivot.y - rotated.y;
}

// =============================================================================
// Generators: layout generation / guessing / verification from two basis vectors
// (one key to the right, one key up-right). The arithmetic is XenCore's.
// =============================================================================

let palette = [];       // array of hex strings (no '#'), length = edo (wraps)
let mismatchSet = new Set();  // pads with values but disagreeing with generators
let missingSet = new Set();   // pads that are unset or off (chan 0)

function currentEdo() {
  const e = parseInt(document.getElementById('edo').value, 10);
  return Number.isFinite(e) && e > 0 ? e : null;
}

// Pad label text for in-grid rendering. Honours the Virtual/Absolute
// display toggle. A key that is off (chan 0) shows "–".
function padLabel(padInfo) {
  if (padInfo.chan === 0) return '–';
  if (displayMode === 'absolute' && currentEdo() !== null) {
    return String(XenCore.pitchOf(padInfo, currentEdo(), layout.pitch_offset));
  }
  return `${padInfo.key}/${padInfo.chan}`;
}

// The generator inputs as XenCore takes them.
function generatorInputs() {
  return {
    gA: parseInt(document.getElementById('genA').value, 10),
    gB: parseInt(document.getElementById('genB').value, 10),
    base: {
      board: document.getElementById('baseBoard').value,
      key: parseInt(document.getElementById('basePad').value, 10),
    },
    baseKey: parseInt(document.getElementById('baseKey').value, 10),
    baseChan: parseInt(document.getElementById('baseChan').value, 10),
  };
}

function populateBaseBoardOptions() {
  const sel = document.getElementById('baseBoard');
  const current = sel.value || 'board0';
  sel.innerHTML = '';
  for (const b of geometry.boards) {
    const o = document.createElement('option');
    o.value = b.name;
    o.textContent = b.name;
    sel.appendChild(o);
  }
  sel.value = geometry.boards.some(b => b.name === current) ? current : geometry.boards[0].name;
}

// Populate the preset dropdown for a generator input.
function populatePresets(selectId, inputId) {
  const edo = currentEdo();
  const sel = document.getElementById(selectId);
  sel.innerHTML = '';
  if (edo === null) return;
  const presets = [
    ['(custom)', null],
    [`semitone (${Math.round(edo / 12)})`, Math.round(edo / 12)],
    [`minor 3rd (${Math.round(edo * Math.log2(6 / 5))})`, Math.round(edo * Math.log2(6 / 5))],
    [`major 3rd (${Math.round(edo * Math.log2(5 / 4))})`, Math.round(edo * Math.log2(5 / 4))],
    [`perfect 4th (${Math.round(edo * Math.log2(4 / 3))})`, Math.round(edo * Math.log2(4 / 3))],
    [`perfect 5th (${Math.round(edo * Math.log2(3 / 2))})`, Math.round(edo * Math.log2(3 / 2))],
    [`octave (${edo})`, edo],
  ];
  for (const [label, value] of presets) {
    const o = document.createElement('option');
    o.value = value === null ? '' : String(value);
    o.textContent = label;
    sel.appendChild(o);
  }
  sel.onchange = () => {
    if (sel.value === '') return;
    document.getElementById(inputId).value = sel.value;
  };
}

function renderPalette() {
  const edo = currentEdo() || 12;
  while (palette.length < edo) palette.push('000000');
  const host = document.getElementById('paletteSwatches');
  host.innerHTML = '';
  for (let i = 0; i < edo; i++) {
    const div = document.createElement('div');
    div.className = 'swatch edit';
    div.style.background = '#' + palette[i];
    div.title = `pitch class ${i}`;
    const inp = document.createElement('input');
    inp.type = 'color';
    inp.value = '#' + palette[i].toLowerCase();
    inp.addEventListener('input', (e) => {
      palette[i] = e.target.value.replace('#', '').toUpperCase();
      div.style.background = '#' + palette[i];
    });
    div.appendChild(inp);
    host.appendChild(div);
  }
}

function guessGenerators() {
  const g = XenCore.guess(geometry, layout);
  if (g.error) { setGenStatus(g.error); return; }
  document.getElementById('genA').value = g.gA;
  document.getElementById('genB').value = g.gB;
  palette = g.palette;
  renderPalette();

  // Use the current base-pad inputs as the origin; read its Key/Chan from the
  // layout if set there, otherwise take the first set pad so the reference is
  // something actually present.
  const base = generatorInputs().base;
  let entry = XenCore.entryOf(layout, base.board, base.key);
  if (XenCore.isMissing(entry)) {
    document.getElementById('baseBoard').value = g.firstSet.board;
    document.getElementById('basePad').value = g.firstSet.key;
    entry = g.firstSet.entry;
  }
  document.getElementById('baseKey').value = entry.key;
  document.getElementById('baseChan').value = entry.chan;

  // Run verify to also populate mismatchSet & missingSet.
  const { totalSet, mismatched, missing } = runVerification();
  setGenStatus(
    `gA=${g.gA}, gB=${g.gB} — fit ${g.fitPct}% over edges · ` +
    `${totalSet} pads set, ${mismatched} non-matching, ${missing} missing`
  );
  renderBoards();
}

function reportApplied(res, label) {
  mismatchSet.clear();
  missingSet.clear();
  if (!res) { setGenStatus(currentEdo() ? 'base pad not found' : 'set Edo first'); return; }
  if (res.count > 0) markDirty();
  const skipped = res.skipped.length ? ` · ${res.skipped.length} left (pitch outside Chan 1-16 / Key 0-127)` : '';
  setGenStatus(`${label}: ${res.count} pads${skipped}`);
  renderBoards();
}

function applyGenerators() {
  reportApplied(XenCore.applyGenerators(geometry, layout, generatorInputs(), palette, null), 'applied to all');
}

// Fill the keys that are missing (unset or off) or do not follow the generators.
function applyGeneratorsRepair() {
  reportApplied(XenCore.repair(geometry, layout, generatorInputs(), palette), 'repaired');
}

// Core verification: populates mismatchSet and missingSet based on current
// generator inputs. Returns counts. Does NOT call renderBoards().
function runVerification() {
  mismatchSet.clear();
  missingSet.clear();
  const v = XenCore.verify(geometry, layout, generatorInputs());
  if (!v) return { totalSet: 0, mismatched: 0, missing: 0 };
  mismatchSet = new Set(v.mismatch);
  missingSet = new Set(v.missing);
  return { totalSet: v.totalSet, mismatched: v.mismatch.length, missing: v.missing.length };
}

function verifyGenerators() {
  const { totalSet, mismatched, missing } = runVerification();
  setGenStatus(`verified: ${totalSet} set, ${mismatched} non-matching, ${missing} missing`);
  renderBoards();
}

function clearMismatchMarkers() {
  mismatchSet.clear();
  missingSet.clear();
  setGenStatus('markers cleared');
  renderBoards();
}

function setGenStatus(s) {
  document.getElementById('genStatus').textContent = s;
}

// ---- Colors in use: tally + recolor ----

function countColorsInUse() {
  const counts = new Map();
  for (const bn of Object.keys(layout.boards || {})) {
    const pads = layout.boards[bn] || {};
    for (const pid of Object.keys(pads)) {
      const col = (pads[pid].color || '000000').toUpperCase();
      counts.set(col, (counts.get(col) || 0) + 1);
    }
  }
  return Array.from(counts.entries())
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

function renderColorsInUse() {
  const host = document.getElementById('colorsInUse');
  if (!host) return;
  // Preserve the help line at top; remove any old rows.
  const oldRows = host.querySelectorAll('.row');
  oldRows.forEach(r => r.remove());
  const entries = countColorsInUse();
  if (entries.length === 0) {
    const row = document.createElement('div');
    row.className = 'row';
    row.innerHTML = '<span class="hex">(no pads set)</span>';
    host.appendChild(row);
    return;
  }
  for (const [hex, count] of entries) {
    const row = document.createElement('div');
    row.className = 'row';

    const sw = document.createElement('div');
    sw.className = 'swatch';
    sw.style.background = '#' + hex;
    sw.title = `${count} pad(s) — click to change all to a new color`;
    const inp = document.createElement('input');
    inp.type = 'color';
    inp.value = '#' + hex.toLowerCase();
    inp.addEventListener('change', (e) => {
      const newHex = e.target.value.replace('#', '').toUpperCase();
      if (newHex === hex) return;
      recolorAll(hex, newHex);
    });
    sw.appendChild(inp);

    const lbl = document.createElement('span');
    lbl.className = 'hex';
    lbl.textContent = '#' + hex;

    const cnt = document.createElement('span');
    cnt.className = 'count';
    cnt.textContent = `${count}`;

    row.appendChild(sw);
    row.appendChild(lbl);
    row.appendChild(cnt);
    host.appendChild(row);
  }
}

function recolorAll(oldHex, newHex) {
  oldHex = oldHex.toUpperCase();
  newHex = newHex.toUpperCase();
  let changed = 0;
  for (const bn of Object.keys(layout.boards || {})) {
    const pads = layout.boards[bn] || {};
    for (const pid of Object.keys(pads)) {
      if ((pads[pid].color || '').toUpperCase() === oldHex) {
        pads[pid].color = newHex;
        changed++;
      }
    }
  }
  if (changed > 0) markDirty();
  setStatus(`recolored ${changed} pad(s) from #${oldHex} to #${newHex}`);
  renderBoards();
  renderColorsInUse();
}

// Wire up Generators UI.
function initGeneratorsUI() {
  populatePresets('genAPreset', 'genA');
  populatePresets('genBPreset', 'genB');
  renderPalette();

  document.getElementById('btnGuess').addEventListener('click', guessGenerators);
  document.getElementById('btnApplyGen').addEventListener('click', applyGenerators);
  document.getElementById('btnRepairGen').addEventListener('click', applyGeneratorsRepair);
  document.getElementById('btnVerifyGen').addEventListener('click', verifyGenerators);
  document.getElementById('btnClearMismatch').addEventListener('click', clearMismatchMarkers);

  document.getElementById('edo').addEventListener('change', () => {
    populatePresets('genAPreset', 'genA');
    populatePresets('genBPreset', 'genB');
    renderPalette();
  });
}

loadAll().then(() => initGeneratorsUI()).catch(e => setStatus('load error: ' + e));
