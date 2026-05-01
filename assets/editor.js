// Xentool Layout Editor — vanilla JS frontend.

const HEX_SIZE = 22;
const SHORT = HEX_SIZE * Math.sqrt(3) / 2;
const LONG = HEX_SIZE * 1.5;

// All three geometries (Exquis, LTN, WTN) share the same doubled-y convention:
// y = horizontal (step 2 between same-row neighbours), x = vertical (row index).
// Exquis places row 0 at the BOTTOM (YRightXUp); LTN/WTN place row 0 at the TOP.
function hexToPixel(x, y, orientation) {
  const px = y * SHORT;
  const py = orientation === 'YRightXUp' ? -x * LONG : x * LONG;
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

// Hex rotation in cube coords. Doubled-y (x=row, y=lateral): cube q=y/2 (floor), r=x.
// Actually this matches xenwooting's rotation logic.
function rotateHex(x, y, steps) {
  let q = Math.trunc((y - x) / 2);
  let r = x;
  let s = -q - r;
  const k = ((steps % 6) + 6) % 6;
  for (let i = 0; i < k; i++) {
    const nq = -r, nr = -s, ns = -q;
    q = nq; r = nr; s = ns;
  }
  return { x: r, y: 2 * q + r };
}

let geometry = null;
let layout = null;
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
    const data = await fetch('/api/files').then(r => r.json());
    currentLayoutName = data.current || '';
    layoutSelectEl.innerHTML = '';
    for (const name of data.files) {
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
  const res = await fetch('/api/load', {
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
  geometry = await fetch('/api/geometry').then(r => r.json());
  layout = await fetch('/api/layout').then(r => r.json());
  document.getElementById('edo').value = layout.edo ?? '';
  document.getElementById('pitchOffset').value = layout.pitch_offset;
  // View-toggle is visible for both kinds. Global import stays hidden for
  // Wooting (no unified hex lattice between separate keyboards).
  const globalImp = document.getElementById('globalImportLabel');
  if (globalImp) globalImp.style.display = geometry.kind === 'wooting' ? 'none' : '';
  document.title = geometry.kind === 'wooting'
    ? 'Wooting Layout Editor'
    : 'Exquis Layout Editor';
  currentLayoutName = geometry.current_file || currentLayoutName;
  clearDirty();
  await populateLayoutSelect();
  renderBoards();
  setStatus('loaded');
}

function setStatus(s) { statusEl.textContent = s; }

function orientationForExquis() {
  return geometry.exquis_orientation || 'YRightXUp';
}

function boardNames() {
  const names = new Set(Object.keys(layout.boards || {}));
  if (names.size === 0) names.add('board0');
  const arr = Array.from(names).sort((a, b) => {
    const na = parseInt(a.replace('board', ''), 10);
    const nb = parseInt(b.replace('board', ''), 10);
    return na - nb;
  });
  return arr;
}

function renderBoards() {
  boardsEl.innerHTML = '';
  const kind = geometry.kind || 'exquis';

  // Wooting: stack vertically, each board/pair full-width, v-scroll for overflow.
  // Exquis: original horizontal flex layout.
  boardsEl.classList.toggle('vertical-stack', kind === 'wooting');

  if (kind === 'wooting') {
    const names = boardNames();
    if (viewMode === 'combined') {
      // Pair even-indexed boards with the following odd one per the
      // xenwooting rotation rule. A trailing odd-count lone board is shown solo.
      let i = 0;
      while (i < names.length) {
        const idx = parseInt(names[i].replace('board', ''), 10);
        const nextIdx = i + 1 < names.length
          ? parseInt(names[i + 1].replace('board', ''), 10)
          : null;
        const shouldPair = (idx % 2 === 0) && (i + 1 < names.length);
        if (shouldPair) {
          boardsEl.appendChild(renderWootingPair(names[i], names[i + 1]));
          i += 2;
        } else {
          boardsEl.appendChild(renderWootingBoard(names[i], idx, false));
          i += 1;
        }
        // Silence unused-var lint (nextIdx retained for clarity above).
        void nextIdx;
      }
    } else {
      for (const name of names) {
        const boardIdx = parseInt(name.replace('board', ''), 10);
        boardsEl.appendChild(renderWootingBoard(name, boardIdx, false));
      }
    }
  } else if (viewMode === 'combined') {
    boardsEl.appendChild(renderCombined());
  } else {
    for (const name of boardNames()) {
      const boardIdx = parseInt(name.replace('board', ''), 10);
      boardsEl.appendChild(renderBoardIndividual(name, boardIdx));
    }
  }
  renderColorsInUse();
}

function renderCombined() {
  const container = document.createElement('div');
  container.className = 'board-container';

  const h = document.createElement('h3');
  h.textContent = 'All boards (combined lattice)';
  container.appendChild(h);

  const names = boardNames();
  const orient = orientationForExquis();

  // Gather all pads in UNIFIED lattice coords.
  const allPads = [];
  for (const name of names) {
    const bi = parseInt(name.replace('board', ''), 10);
    const tuples = geometry.exquis_boards[bi] || [];
    for (const t of tuples) {
      allPads.push({ board: name, pad: t.key, x: t.x, y: t.y });
    }
  }

  const { minPx, maxPx, minPy, maxPy } = computeBounds(
    allPads.map(p => ({ x: p.x, y: p.y })), orient
  );
  const pad = HEX_SIZE + 4;
  const width = (maxPx - minPx) + 2 * pad;
  const height = (maxPy - minPy) + 2 * pad;
  const ox = -minPx + pad;
  const oy = -minPy + pad;

  const svg = makeSvg(width, height);

  // Render each pad using its UNIFIED coords (no translation back).
  for (const p of allPads) {
    const padInfo = (layout.boards[p.board] && layout.boards[p.board][p.pad]) || {
      key: p.pad, chan: 1, color: '000000'
    };
    drawPad(svg, padInfo, p.board, p.pad, p.x, p.y, ox, oy, orient);
  }

  // Import overlay for combined view.
  if (importState && importState.scope === 'global') {
    drawImportOverlayCombined(svg, ox, oy, orient);
  }

  container.appendChild(svg);
  return container;
}

function renderBoardIndividual(name, boardIdx) {
  const container = document.createElement('div');
  container.className = 'board-container';

  const h = document.createElement('h3');
  const label = document.createElement('span');
  label.textContent = name;
  h.appendChild(label);

  // Per-board import button (only active in individual mode).
  const importLbl = document.createElement('label');
  importLbl.className = 'board-header-import';
  importLbl.textContent = 'Import…';
  const importInput = document.createElement('input');
  importInput.type = 'file';
  importInput.accept = '.ltn,.wtn,.xtn';
  importInput.style.display = 'none';
  importInput.addEventListener('change', (e) => handleImport(e.target.files[0], name));
  importLbl.appendChild(importInput);
  h.appendChild(importLbl);

  container.appendChild(h);

  const orient = orientationForExquis();
  const tuples = geometry.exquis_boards[boardIdx] || geometry.exquis_boards[0];

  // Convert to LOCAL coords by subtracting stride.
  const strideX = boardIdx * geometry.exquis_board_stride_x;
  const strideY = boardIdx * geometry.exquis_board_stride_y;
  const localPads = tuples.map(t => ({
    pad: t.key,
    localX: t.x - strideX,
    localY: t.y - strideY,
  }));

  const { minPx, maxPx, minPy, maxPy } = computeBounds(
    localPads.map(p => ({ x: p.localX, y: p.localY })), orient
  );
  const pad = HEX_SIZE + 4;
  const width = (maxPx - minPx) + 2 * pad;
  const height = (maxPy - minPy) + 2 * pad;
  const ox = -minPx + pad;
  const oy = -minPy + pad;

  const svg = makeSvg(width, height);

  for (const p of localPads) {
    const padInfo = (layout.boards[name] && layout.boards[name][p.pad]) || {
      key: p.pad, chan: 1, color: '000000'
    };
    // Draw at local coords but store the absolute info in dataset for selection.
    drawPad(svg, padInfo, name, p.pad, p.localX, p.localY, ox, oy, orient);
  }

  // Per-board import overlay.
  if (importState && importState.scope === name) {
    drawImportOverlayPerBoard(svg, boardIdx, ox, oy, orient);
  }

  container.appendChild(svg);
  return container;
}

// Build one responsive Wooting keyboard canvas (keys in percent coords so
// the whole canvas scales with its container while preserving aspect ratio).
// Per-row minimum col after rotation. Matches xenwooting's minColByRow so
// rotated boards' WTN indices are compacted correctly.
function wtnMinColByRow(keys, rotate180) {
  const min = [255, 255, 255, 255];
  for (const k of keys) {
    const rr = rotate180 ? 3 - k.row : k.row;
    const cc = rotate180 ? 13 - k.col : k.col;
    if (rr >= 0 && rr < 4 && cc < min[rr]) min[rr] = cc;
  }
  for (let i = 0; i < 4; i++) if (min[i] === 255) min[i] = 0;
  return min;
}

// Compute the WTN cell index (0..55) for an ANSI key `k`, honoring rotation
// and the rotation-aware compact-col offsets.
function wtnIdxForKey(k, rotate180, minColByRow) {
  const rr = rotate180 ? 3 - k.row : k.row;
  const cc0 = rotate180 ? 13 - k.col : k.col;
  const cc = cc0 - (minColByRow[rr] || 0);
  if (rr < 0 || rr >= 4 || cc < 0 || cc >= 14) return null;
  return rr * 14 + cc;
}

function createWootingCanvas(name, rotate180) {
  const keys = geometry.wooting_keys || [];
  const bw = geometry.wooting_board_width;
  const bh = geometry.wooting_board_height;
  const minColByRow = wtnMinColByRow(keys, rotate180);

  const canvas = document.createElement('div');
  canvas.className = 'kbd-canvas';
  canvas.style.setProperty('--aspect', `${bw} / ${bh}`);

  for (const k of keys) {
    const wtnIdx = wtnIdxForKey(k, rotate180, minColByRow);
    const padInfo =
      (wtnIdx !== null && layout.boards[name] && layout.boards[name][wtnIdx]) || {
        key: wtnIdx ?? k.idx, chan: 0, color: '000000'
      };
    const x0 = rotate180 ? (bw - (k.x + k.w)) : k.x;
    const y0 = rotate180 ? (bh - (k.y + k.h)) : k.y;
    const btn = document.createElement('button');
    btn.className = 'kbd-rect';
    btn.style.position = 'absolute';
    btn.style.left   = (x0 / bw * 100) + '%';
    btn.style.top    = (y0 / bh * 100) + '%';
    btn.style.width  = (k.w / bw * 100) + '%';
    btn.style.height = (k.h / bh * 100) + '%';
    btn.style.backgroundColor = '#' + padInfo.color;
    btn.style.color = isDark(padInfo.color) ? '#fff' : '#000';
    btn.title = `row ${k.row} col ${k.col} — wtn idx ${wtnIdx}`;

    const selKeyIdx = wtnIdx ?? k.idx;
    const selKey = `${name}:${selKeyIdx}`;
    if (selection.has(selKey)) btn.classList.add('selected');
    if (mismatchSet.has(selKey)) btn.classList.add('mismatch');
    else if (missingSet.has(selKey)) btn.classList.add('missing');

    btn.textContent = padLabel(padInfo, true);
    btn.addEventListener('click', (e) => onPadClick(e, name, selKeyIdx));
    canvas.appendChild(btn);
  }
  return canvas;
}

function renderWootingBoard(name, boardIdx, rotate180) {
  const container = document.createElement('div');
  container.className = 'board-container';
  const h = document.createElement('h3');
  const label = document.createElement('span');
  label.textContent = `${name}${rotate180 ? ' (rotated 180°)' : ''}`;
  h.appendChild(label);

  const importLbl = document.createElement('label');
  importLbl.className = 'board-header-import';
  importLbl.textContent = 'Import…';
  const importInput = document.createElement('input');
  importInput.type = 'file';
  importInput.accept = '.ltn,.wtn,.xtn';
  importInput.style.display = 'none';
  importInput.addEventListener('change', (e) => handleImport(e.target.files[0], name));
  importLbl.appendChild(importInput);
  h.appendChild(importLbl);
  container.appendChild(h);

  container.appendChild(createWootingCanvas(name, rotate180));
  // Silence lint; boardIdx kept in signature for call-site clarity.
  void boardIdx;
  return container;
}

// Combined view for a pair: rotated board on top (horizontally shifted right
// by the pair-top offset so the musical lattice lines up with the bottom
// board), upright board on bottom. Both canvases share one board-container.
// Matches xenwooting's physical layout where an even-indexed board is
// flipped 180° and placed above its partner with a 2-unit right shift.
function renderWootingPair(rotatedName, uprightName) {
  const container = document.createElement('div');
  container.className = 'board-container wooting-pair';
  const h = document.createElement('h3');
  h.textContent = `${rotatedName} (rotated 180°) + ${uprightName}`;
  container.appendChild(h);

  const bw = geometry.wooting_board_width;
  const shift = geometry.wooting_pair_top_x_shift || 0;
  const total = bw + shift;
  const canvasPct = (bw / total) * 100;
  const shiftPct = (shift / total) * 100;
  // Each sub-canvas is `bw / (bw+shift)` of the container width; the top one
  // also has a left margin equal to `shift / (bw+shift)` of the container.
  const top = createWootingCanvas(rotatedName, true);
  top.style.width = canvasPct + '%';
  top.style.marginLeft = shiftPct + '%';
  const bot = createWootingCanvas(uprightName, false);
  bot.style.width = canvasPct + '%';
  bot.style.marginLeft = '0';
  container.appendChild(top);
  container.appendChild(bot);
  return container;
}

function computeBounds(points, orient) {
  let minPx = Infinity, maxPx = -Infinity, minPy = Infinity, maxPy = -Infinity;
  for (const p of points) {
    const { px, py } = hexToPixel(p.x, p.y, orient);
    if (px < minPx) minPx = px;
    if (px > maxPx) maxPx = px;
    if (py < minPy) minPy = py;
    if (py > maxPy) maxPy = py;
  }
  return { minPx, maxPx, minPy, maxPy };
}

function makeSvg(width, height) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.setAttribute('class', 'board-svg');
  return svg;
}

function drawPad(svg, padInfo, boardName, padId, hx, hy, ox, oy, orient) {
  const { px, py } = hexToPixel(hx, hy, orient);
  const cx = px + ox;
  const cy = py + oy;

  const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
  g.dataset.board = boardName;
  g.dataset.pad = padId;

  const poly = document.createElementNS('http://www.w3.org/2000/svg', 'polygon');
  poly.setAttribute('points', hexPolyPoints(cx, cy, HEX_SIZE));
  poly.setAttribute('fill', '#' + padInfo.color);
  poly.setAttribute('class', 'pad');
  const selKey = `${boardName}:${padId}`;
  if (selection.has(selKey)) poly.classList.add('selected');
  g.appendChild(poly);

  // Mismatch marker: red dashed border overlay. Missing: grey dashed border.
  if (mismatchSet.has(selKey)) {
    const mark = document.createElementNS('http://www.w3.org/2000/svg', 'polygon');
    mark.setAttribute('points', hexPolyPoints(cx, cy, HEX_SIZE - 2));
    mark.setAttribute('class', 'pad-mismatch');
    g.appendChild(mark);
  } else if (missingSet.has(selKey)) {
    const mark = document.createElementNS('http://www.w3.org/2000/svg', 'polygon');
    mark.setAttribute('points', hexPolyPoints(cx, cy, HEX_SIZE - 2));
    mark.setAttribute('class', 'pad-missing');
    g.appendChild(mark);
  }

  const lbl = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  lbl.setAttribute('x', cx);
  lbl.setAttribute('y', cy + 3);
  lbl.setAttribute('class', 'pad-label');
  if (isDark(padInfo.color)) lbl.classList.add('light');
  lbl.textContent = padLabel(padInfo, false);
  g.appendChild(lbl);

  const padIdLbl = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  padIdLbl.setAttribute('x', cx);
  padIdLbl.setAttribute('y', cy - HEX_SIZE * 0.55);
  padIdLbl.setAttribute('class', 'pad-label');
  if (isDark(padInfo.color)) padIdLbl.classList.add('light');
  padIdLbl.setAttribute('style', 'font-size: 8px; opacity: 0.7');
  padIdLbl.textContent = `#${padId}`;
  g.appendChild(padIdLbl);

  g.addEventListener('click', (e) => onPadClick(e, boardName, padId));
  g.addEventListener('mouseenter', () => {
    // For hover tracking during import rotation, we need UNIFIED coords.
    // Recompute from board stride if in individual view.
    const bi = parseInt(boardName.replace('board', ''), 10);
    const unifiedX = viewMode === 'combined' ? hx : hx + bi * geometry.exquis_board_stride_x;
    const unifiedY = viewMode === 'combined' ? hy : hy + bi * geometry.exquis_board_stride_y;
    hoveredPad = { board: boardName, pad: padId, x: unifiedX, y: unifiedY };
  });
  g.addEventListener('mouseleave', () => {
    if (hoveredPad && hoveredPad.board === boardName && hoveredPad.pad === padId) hoveredPad = null;
  });
  svg.appendChild(g);
}

function drawImportOverlayCombined(svg, ox, oy, orient) {
  // Build lookup: unified (x,y) → exists
  const unifiedXY = new Set();
  for (let b = 0; b < geometry.exquis_boards.length; b++) {
    for (const t of geometry.exquis_boards[b]) unifiedXY.add(`${t.x},${t.y}`);
  }
  for (const p of importState.pads) {
    const { x, y } = projectImportPad(p);
    if (!unifiedXY.has(`${x},${y}`)) continue;
    drawOverlayCell(svg, x, y, ox, oy, orient, `${p.key}/${p.chan}`);
  }
}

function drawImportOverlayPerBoard(svg, boardIdx, ox, oy, orient) {
  const tuples = geometry.exquis_boards[boardIdx] || [];
  const strideX = boardIdx * geometry.exquis_board_stride_x;
  const strideY = boardIdx * geometry.exquis_board_stride_y;
  const localSet = new Set();
  for (const t of tuples) localSet.add(`${t.x},${t.y}`);
  for (const p of importState.pads) {
    const { x, y } = projectImportPad(p);
    if (!localSet.has(`${x},${y}`)) continue;
    drawOverlayCell(svg, x - strideX, y - strideY, ox, oy, orient, `${p.key}/${p.chan}`);
  }
}

function drawOverlayCell(svg, hx, hy, ox, oy, orient, text) {
  const { px, py } = hexToPixel(hx, hy, orient);
  const cx = px + ox;
  const cy = py + oy;
  const poly = document.createElementNS('http://www.w3.org/2000/svg', 'polygon');
  poly.setAttribute('points', hexPolyPoints(cx, cy, HEX_SIZE - 3));
  poly.setAttribute('class', 'import-overlay');
  svg.appendChild(poly);
  const lbl = document.createElementNS('http://www.w3.org/2000/svg', 'text');
  lbl.setAttribute('x', cx);
  lbl.setAttribute('y', cy + HEX_SIZE * 0.75);
  lbl.setAttribute('class', 'import-label');
  lbl.textContent = text;
  svg.appendChild(lbl);
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
    const p = (layout.boards[bn] || {})[pid] || { key: +pid, chan: 1, color: '000000' };
    info.textContent = `${bn} #${pid}: Key=${p.key} Chan=${p.chan} Col=#${p.color}`;
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
  for (const sel of selection) {
    const [bn, pidStr] = sel.split(':');
    ensureBoard(bn)[parseInt(pidStr, 10)] = { key, chan, color };
  }
  markDirty();
  setStatus(`applied to ${selection.size} pads`);
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
});
document.getElementById('pitchOffset').addEventListener('change', (e) => {
  layout.pitch_offset = parseInt(e.target.value, 10) || 0;
  markDirty();
});

document.getElementById('save').addEventListener('click', async () => {
  const res = await fetch('/api/layout', {
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

async function handleImport(file, scope) {
  if (!file) return;
  resetImport();
  const text = await file.text();
  const ext = file.name.split('.').pop().toLowerCase();
  const res = await fetch('/api/import', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content: text, kind: ext }),
  });
  if (!res.ok) {
    setStatus('import failed: ' + await res.text());
    return;
  }
  const data = await res.json();
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

  // Determine target boards whose non-imported pads will be reset to black/0/0.
  const targetBoards = importState.scope === 'global'
    ? geometry.exquis_boards.map((_, i) => `board${i}`)
    : [importState.scope];

  // Reset every pad on target boards to (key=0, chan=0, color=black).
  for (const bn of targetBoards) {
    const boardIdx = parseInt(bn.replace('board', ''), 10);
    const tuples = geometry.exquis_boards[boardIdx] || [];
    const pads = ensureBoard(bn);
    for (const t of tuples) {
      pads[t.key] = { key: 0, chan: 0, color: '000000' };
    }
  }

  // Apply projected imported cells on top of the cleared boards.
  let applied = 0;
  const xyToBoard = new Map();
  for (let b = 0; b < geometry.exquis_boards.length; b++) {
    for (const t of geometry.exquis_boards[b]) {
      xyToBoard.set(`${t.x},${t.y}`, { board: `board${b}`, pad: t.key });
    }
  }
  for (const p of importState.pads) {
    const { x, y } = projectImportPad(p);
    const target = xyToBoard.get(`${x},${y}`);
    if (!target) continue;
    // In per-board mode, ignore cells that fall on other boards.
    if (importState.scope !== 'global' && target.board !== importState.scope) continue;
    ensureBoard(target.board)[target.pad] = { key: p.key, chan: p.chan, color: p.color };
    applied++;
  }
  resetImport();
  markDirty();
  setStatus(`applied ${applied} pads from import; rest reset to 0/0/black`);
  renderBoards();
});

function projectImportPad(p) {
  const rotated = rotateHex(p.x, p.y, importState.rot);
  return { x: rotated.x + importState.tx, y: rotated.y + importState.ty };
}

// ---- Keyboard handlers (for import mode) ----

document.addEventListener('keydown', (e) => {
  if (e.target && (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA')) return;
  if (!importState) return;

  // In our convention: x is vertical, y is horizontal. Exquis has YRightXUp,
  // so increasing x moves UP visually.
  const orient = orientationForExquis();
  const upKey   = orient === 'YRightXUp' ? 'ArrowUp'   : 'ArrowDown';
  const downKey = orient === 'YRightXUp' ? 'ArrowDown' : 'ArrowUp';

  if (e.key === 'ArrowLeft')     { importState.ty -= 2; renderBoards(); e.preventDefault(); }
  else if (e.key === 'ArrowRight') { importState.ty += 2; renderBoards(); e.preventDefault(); }
  else if (e.key === upKey)   { importState.tx += 1; importState.ty += 1; renderBoards(); e.preventDefault(); }
  else if (e.key === downKey) { importState.tx -= 1; importState.ty -= 1; renderBoards(); e.preventDefault(); }
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
  const rotated = rotateHex(pivotMinusT.x, pivotMinusT.y, 1);
  importState.rot = (importState.rot + 1) % 6;
  importState.tx = pivot.x - rotated.x;
  importState.ty = pivot.y - rotated.y;
}

// =============================================================================
// Generators: layout generation / guessing / verification from two basis vectors.
// =============================================================================

// Natural hex basis directions in our doubled-y convention.
const DIR_A = [0, 2];   // right along a row
const DIR_B = [1, 1];   // up-right diagonal

let palette = [];       // array of hex strings (no '#'), length = edo (wraps)
let mismatchSet = new Set();  // pads with values but disagreeing with generators
let missingSet = new Set();   // pads that are unset (chan=0 is our "empty" marker)

function isMissingEntry(entry) {
  // A pad is treated as "missing" if it has no entry or chan === 0 (our canonical blank).
  return !entry || entry.chan === 0;
}

function currentEdo() {
  const e = parseInt(document.getElementById('edo').value, 10);
  return Number.isFinite(e) && e > 0 ? e : null;
}

// Pad label text for in-grid rendering. Honours the Virtual/Absolute
// display toggle. `inactiveAsDash` lets the Wooting branch keep its
// existing convention of showing "–" for chan==0 (inactive key).
function padLabel(padInfo, inactiveAsDash) {
  if (inactiveAsDash && padInfo.chan === 0) return '–';
  if (displayMode === 'absolute') {
    const p = pitchOfEntry(padInfo);
    if (p !== null) return String(p);
    // No EDO set yet — fall through to virtual so the user still sees something.
  }
  return `${padInfo.key}/${padInfo.chan}`;
}

// Compute abs_pitch for a pad entry.
function pitchOfEntry(entry) {
  if (!entry) return null;
  const edo = currentEdo();
  if (edo === null) return null;
  return (entry.chan - 1) * edo + entry.key + (layout.pitch_offset || 0);
}

// Convert signed abs_pitch back to (chan, key), clamped.
function pitchToChanKey(absPitch) {
  const edo = currentEdo();
  if (edo === null) return { chan: 1, key: 0 };
  const withOff = absPitch - (layout.pitch_offset || 0);
  let chan = Math.floor(withOff / edo) + 1;
  let key = withOff - (chan - 1) * edo;
  // Clamp chan to 1..16, overflow key.
  if (chan < 1) { key += (chan - 1) * edo; chan = 1; }
  if (chan > 16) { key += (chan - 16) * edo; chan = 16; }
  key = Math.max(0, Math.min(127, key));
  return { chan, key };
}

// Hex basis decomposition: (dx, dy) = dq*DIR_A + dr*DIR_B
function hexBasisCoeffs(dx, dy) {
  // DIR_A = (0, 2), DIR_B = (1, 1). Solve:
  //   dx = 0*dq + 1*dr  → dr = dx
  //   dy = 2*dq + 1*dr  → dq = (dy - dr) / 2 = (dy - dx) / 2
  return { dq: (dy - dx) / 2, dr: dx };
}

// For a pad at (hexX, hexY), compute predicted abs_pitch given generators + base.
function predictedPitch(hexX, hexY, bxy, gA, gB, basePitch) {
  const dx = hexX - bxy[0];
  const dy = hexY - bxy[1];
  const { dq, dr } = hexBasisCoeffs(dx, dy);
  return basePitch + dq * gA + dr * gB;
}

function boardBase() {
  const bn = document.getElementById('baseBoard').value;
  const pid = parseInt(document.getElementById('basePad').value, 10);
  const bi = parseInt(bn.replace('board', ''), 10);
  const tuples = geometry.exquis_boards[bi] || [];
  const t = tuples.find(tt => tt.key === pid);
  if (!t) return null;
  return { board: bn, pad: pid, hx: t.x, hy: t.y };
}

function populateBaseBoardOptions() {
  const sel = document.getElementById('baseBoard');
  const current = sel.value || 'board0';
  sel.innerHTML = '';
  for (let i = 0; i < geometry.exquis_boards.length; i++) {
    const o = document.createElement('option');
    o.value = `board${i}`;
    o.textContent = `board${i}`;
    sel.appendChild(o);
  }
  sel.value = current;
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
  sel.addEventListener('change', () => {
    if (sel.value === '') return;
    document.getElementById(inputId).value = sel.value;
  });
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

// Build unified (x,y) → {board, pad, entry} map of pads that are *set*
// (missing/empty pads are skipped so they don't influence the best-fit).
function unifiedPadMap() {
  const map = new Map();
  for (const bn of boardNames()) {
    const bi = parseInt(bn.replace('board', ''), 10);
    const tuples = geometry.exquis_boards[bi] || [];
    for (const t of tuples) {
      const p = (layout.boards[bn] || {})[t.key];
      if (isMissingEntry(p)) continue;
      map.set(`${t.x},${t.y}`, { board: bn, pad: t.key, hx: t.x, hy: t.y, entry: p });
    }
  }
  return map;
}

// Find the mode of pitch deltas along direction (dx, dy).
function modeOfDeltas(padMap, dx, dy) {
  const counts = new Map();
  const edges = [];
  for (const [xy, info] of padMap) {
    const [xs, ys] = xy.split(',').map(Number);
    const to = padMap.get(`${xs + dx},${ys + dy}`);
    if (!to) continue;
    const fromPitch = pitchOfEntry(info.entry);
    const toPitch = pitchOfEntry(to.entry);
    if (fromPitch === null || toPitch === null) continue;
    const dp = toPitch - fromPitch;
    counts.set(dp, (counts.get(dp) || 0) + 1);
    edges.push({ from: info, to, dp });
  }
  let best = null, bestCount = -1;
  for (const [dp, c] of counts) if (c > bestCount) { best = dp; bestCount = c; }
  return { best, bestCount, totalEdges: edges.length, edges };
}

function guessGenerators() {
  const edo = currentEdo();
  if (!edo) { setGenStatus('set Edo first'); return; }
  const map = unifiedPadMap();
  if (map.size === 0) { setGenStatus('no pads set (all empty)'); return; }

  const a = modeOfDeltas(map, DIR_A[0], DIR_A[1]);
  const b = modeOfDeltas(map, DIR_B[0], DIR_B[1]);
  if (a.best === null || b.best === null) {
    setGenStatus('not enough adjacent set pads to guess');
    return;
  }
  document.getElementById('genA').value = a.best;
  document.getElementById('genB').value = b.best;

  // Guess palette: for each pitch class, most-common color across SET pads.
  palette = [];
  for (let pc = 0; pc < edo; pc++) palette.push('000000');
  const colorCounts = Array.from({ length: edo }, () => new Map());
  for (const info of map.values()) {
    const p = pitchOfEntry(info.entry);
    const pc = ((p % edo) + edo) % edo;
    const c = info.entry.color || '000000';
    colorCounts[pc].set(c, (colorCounts[pc].get(c) || 0) + 1);
  }
  for (let pc = 0; pc < edo; pc++) {
    let best = '000000', bestCount = 0;
    for (const [c, n] of colorCounts[pc]) if (n > bestCount) { best = c; bestCount = n; }
    palette[pc] = best;
  }
  renderPalette();

  // Use the current base-pad inputs as the origin; read its Key/Chan from the
  // layout if set there, otherwise pick the first set pad's position/pitch so
  // the fit reference is something actually present.
  const base = boardBase();
  if (base) {
    const entry = (layout.boards[base.board] || {})[base.pad];
    if (entry && !isMissingEntry(entry)) {
      document.getElementById('baseKey').value = entry.key;
      document.getElementById('baseChan').value = entry.chan;
    } else {
      // Fallback: use any set pad as the base reference.
      const [, firstInfo] = map.entries().next().value;
      document.getElementById('baseBoard').value = firstInfo.board;
      document.getElementById('basePad').value = firstInfo.pad;
      document.getElementById('baseKey').value = firstInfo.entry.key;
      document.getElementById('baseChan').value = firstInfo.entry.chan;
    }
  }

  // Run verify to also populate mismatchSet & missingSet.
  const { totalSet, mismatched, missing } = runVerification();

  const totalEdges = a.totalEdges + b.totalEdges;
  const matchingEdges = a.bestCount + b.bestCount;
  const pct = totalEdges ? Math.round(1000 * matchingEdges / totalEdges) / 10 : 100;
  setGenStatus(
    `gA=${a.best}, gB=${b.best} — fit ${pct}% over edges · ` +
    `${totalSet} pads set, ${mismatched} non-matching, ${missing} missing`
  );
  renderBoards();
}

// Shared apply logic; `filter(boardName, padId)` returns true to overwrite.
function applyGeneratorsFiltered(filter, label) {
  const edo = currentEdo();
  if (!edo) { setGenStatus('set Edo first'); return; }
  const gA = parseInt(document.getElementById('genA').value, 10);
  const gB = parseInt(document.getElementById('genB').value, 10);
  const base = boardBase();
  if (!base) { setGenStatus('base pad not found'); return; }
  const baseKey = parseInt(document.getElementById('baseKey').value, 10);
  const baseChan = parseInt(document.getElementById('baseChan').value, 10);
  const basePitch = (baseChan - 1) * edo + baseKey + (layout.pitch_offset || 0);

  let count = 0;
  for (let bi = 0; bi < geometry.exquis_boards.length; bi++) {
    const bn = `board${bi}`;
    const tuples = geometry.exquis_boards[bi];
    const pads = ensureBoard(bn);
    for (const t of tuples) {
      if (!filter(bn, t.key)) continue;
      const p = predictedPitch(t.x, t.y, [base.hx, base.hy], gA, gB, basePitch);
      const { chan, key } = pitchToChanKey(p);
      const pc = (((p - (layout.pitch_offset || 0)) % edo) + edo) % edo;
      const color = palette[pc] || '000000';
      pads[t.key] = { key, chan, color };
      count++;
    }
  }
  mismatchSet.clear();
  missingSet.clear();
  if (count > 0) markDirty();
  setGenStatus(`${label}: ${count} pads`);
  renderBoards();
}

function applyGenerators() {
  applyGeneratorsFiltered(() => true, 'applied to all');
}

function applyGeneratorsRepair() {
  // Refresh verification first so we know what's currently wrong.
  runVerification();
  applyGeneratorsFiltered((bn, pad) => {
    const k = `${bn}:${pad}`;
    return mismatchSet.has(k) || missingSet.has(k);
  }, 'repaired');
}

// Core verification: populates mismatchSet and missingSet based on current
// generator inputs. Returns counts. Does NOT call renderBoards().
function runVerification() {
  mismatchSet.clear();
  missingSet.clear();

  const edo = currentEdo();
  if (!edo) return { totalSet: 0, mismatched: 0, missing: 0 };
  const gA = parseInt(document.getElementById('genA').value, 10);
  const gB = parseInt(document.getElementById('genB').value, 10);
  const base = boardBase();
  if (!base) return { totalSet: 0, mismatched: 0, missing: 0 };
  const baseKey = parseInt(document.getElementById('baseKey').value, 10);
  const baseChan = parseInt(document.getElementById('baseChan').value, 10);
  const basePitch = (baseChan - 1) * edo + baseKey + (layout.pitch_offset || 0);

  let totalSet = 0, mismatched = 0, missing = 0;
  for (let bi = 0; bi < geometry.exquis_boards.length; bi++) {
    const bn = `board${bi}`;
    const tuples = geometry.exquis_boards[bi];
    for (const t of tuples) {
      const entry = (layout.boards[bn] || {})[t.key];
      if (isMissingEntry(entry)) {
        missingSet.add(`${bn}:${t.key}`);
        missing++;
        continue;
      }
      totalSet++;
      const expected = predictedPitch(t.x, t.y, [base.hx, base.hy], gA, gB, basePitch);
      const actual = (entry.chan - 1) * edo + entry.key + (layout.pitch_offset || 0);
      if (expected !== actual) {
        mismatchSet.add(`${bn}:${t.key}`);
        mismatched++;
      }
    }
  }
  return { totalSet, mismatched, missing };
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
  populateBaseBoardOptions();
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
