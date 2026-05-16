// tanpura_studio UI driver — vanilla JS, no framework.
//
// On startup, fetch /api/state to get current values + defaults, render
// accordion sections with sliders, hook up change handlers that POST to
// /api/set. Save / Load / Reset are header buttons.

(function () {
  'use strict';

  // Section + control config. Each control: { name, label, min, max, step,
  // format(value) }. Default value comes from /api/state.defaults.
  var SECTIONS = [
    {
      id: 'string',
      title: 'String (KS body)',
      open: true,
      controls: [
        { name: 'decay',       label: 'Decay (KS)',              min: 0.5, max: 30,  step: 0.1,  fmt: v => v.toFixed(1) + ' s' },
        { name: 'dampScale',   label: 'Damp scale',              min: 0.3, max: 2.5, step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'brightScale', label: 'Body / brightness scale', min: 0.3, max: 3.0, step: 0.01, fmt: v => v.toFixed(2) },
      ],
    },
    {
      id: 'drone',
      title: 'Drone (CombL sustain)',
      open: true,
      controls: [
        { name: 'droneAmt', label: 'Drone amount (× white-noise excitation)', min: 0, max: 2, step: 0.01, fmt: v => v.toFixed(2) },
        { type: 'mode', name: 'droneType', label: 'Drone type', options: [
          { value: 0, text: 'CombL feedback (default)' },
          { value: 1, text: 'Sine + harmonics (Y morphs)' },
          { value: 2, text: 'Off' },
          { value: 3, text: 'Saw (Y-filtered)' },
          { value: 4, text: 'Triangle (Y-filtered)' },
          { value: 5, text: 'Beating sines (~7 c)' },
          { value: 6, text: 'Pulse (Y width + filter)' },
          { value: 7, text: 'Vocal "ahh" (vowel formants, Y opens vowel)' },
        ]},
        { name: 'droneHarmGain', label: 'Harmonic gain (Sine+harm Y morph strength)', min: 0, max: 6, step: 0.05, fmt: v => v.toFixed(2), showWhen: s => (s.droneType | 0) === 1 },
      ],
    },
    {
      id: 'jawari',
      title: 'Jawari (nonlinearity)',
      open: true,
      controls: [
        { name: 'jawariAmt',   label: 'Jawari wet/dry mix (0=dry, 0.7=v1, 1=full)', min: 0,   max: 1,    step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'jawariDrive', label: 'Pre-tanh drive coefficient',                 min: 0,   max: 12,   step: 0.1,  fmt: v => v.toFixed(1) },
        { name: 'jawariCubed', label: 'Cubed harmonic mix',                         min: 0,   max: 4,    step: 0.05, fmt: v => v.toFixed(2) },
        { type: 'mode', name: 'jawariMode', label: 'Jawari shape', options: [
          { value: 0, text: 'tanh + cubed (default)' },
          { value: 1, text: 'tanh only (softer)' },
          { value: 2, text: 'Wave fold' },
        ]},
      ],
    },
    {
      id: 'symp',
      title: 'Sympathy (DelayC)',
      open: true,
      controls: [
        { name: 'sympAmt', label: 'Sympathy mix (0=bypass, 1=v1)', min: 0, max: 1, step: 0.01, fmt: v => v.toFixed(2) },
      ],
    },
    {
      id: 'eq',
      title: 'Y-axis tonal effect',
      open: true,
      controls: [
        { type: 'mode', name: 'yMode', label: 'Y-axis effect', options: [
          { value: 0, text: 'EQ shelves (default)' },
          { value: 1, text: 'Resonant LPF sweep (sitar/sarod wah)' },
          { value: 2, text: 'Bandpass formant (oud / vocal)' },
          { value: 3, text: 'Tremolo (saz / kanun shimmer)' },
          { value: 4, text: 'Pluck-position comb (sitar pluck)' },
          { value: 5, text: 'Off' },
        ]},
        { name: 'hiShelfFreq', label: 'High-shelf corner freq',  min: 500,  max: 12000, step: 50,  fmt: v => v.toFixed(0) + ' Hz', showWhen: s => (s.yMode | 0) === 0 },
        { name: 'hiShelfMin',  label: 'High-shelf gain at bright=0', min: -24, max: 24,    step: 1,   fmt: v => v.toFixed(0) + ' dB', showWhen: s => (s.yMode | 0) === 0 },
        { name: 'hiShelfMax',  label: 'High-shelf gain at bright=1', min: -24, max: 24,    step: 1,   fmt: v => v.toFixed(0) + ' dB', showWhen: s => (s.yMode | 0) === 0 },
        { type: 'mode', name: 'hiShelfMode', label: 'High-shelf type', showWhen: s => (s.yMode | 0) === 0, options: [
          { value: 0, text: 'Shelf (default)' },
          { value: 1, text: 'Peaking EQ at corner' },
          { value: 2, text: 'Off' },
        ]},
        { name: 'loShelfFreq', label: 'Low-shelf corner freq',   min: 100,  max: 1500,  step: 10,  fmt: v => v.toFixed(0) + ' Hz', showWhen: s => (s.yMode | 0) === 0 },
        { name: 'loShelfMin',  label: 'Low-shelf gain at bright=0',  min: -24, max: 24,    step: 1,   fmt: v => v.toFixed(0) + ' dB', showWhen: s => (s.yMode | 0) === 0 },
        { name: 'loShelfMax',  label: 'Low-shelf gain at bright=1',  min: -24, max: 24,    step: 1,   fmt: v => v.toFixed(0) + ' dB', showWhen: s => (s.yMode | 0) === 0 },
        { type: 'mode', name: 'loShelfMode', label: 'Low-shelf type', showWhen: s => (s.yMode | 0) === 0, options: [
          { value: 0, text: 'Shelf (default)' },
          { value: 1, text: 'Peaking EQ at corner' },
          { value: 2, text: 'Off' },
        ]},
      ],
    },
    {
      id: 'dynamics',
      title: 'Dynamics (Z-axis swell + limiter)',
      open: true,
      controls: [
        { name: 'pressSwellLo',  label: 'Amp swell at press=0', min: 0,   max: 2,   step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'pressSwellHi',  label: 'Amp swell at press=1', min: 0,   max: 4,   step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'limiterThresh', label: 'Limiter threshold',    min: 0.3, max: 1.5, step: 0.01, fmt: v => v.toFixed(2) },
      ],
    },
    {
      id: 'y_mapping',
      title: 'Y-axis mapping',
      open: true,
      controls: [
        { name: 'yMin',        label: 'Effect at Y=0 (low end of Y range)',                 min: 0,  max: 1,    step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'yCenter',     label: 'Effect at Y=0.5 (curve mid-point)',                  min: 0,  max: 1,    step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'yMax',        label: 'Effect at Y=1 (high end of Y range)',                min: 0,  max: 1,    step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'yPitchTrack', label: 'Pitch attenuation (high notes weaken effect)',       min: 0,  max: 1,    step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'yPitchRefHz', label: 'Pitch reference (Hz; attenuation starts above)',     min: 50, max: 2000, step: 1,    fmt: v => v.toFixed(0) + ' Hz' },
      ],
    },
    {
      id: 'output',
      title: 'Reverb & master',
      open: true,
      controls: [
        { name: 'reverbMix',  label: 'Reverb mix',  min: 0, max: 1, step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'reverbRoom', label: 'Reverb room', min: 0, max: 1, step: 0.01, fmt: v => v.toFixed(2) },
        { name: 'masterAmp',  label: 'Master amp',  min: 0, max: 2, step: 0.01, fmt: v => v.toFixed(2) },
      ],
    },
  ];

  var state = {};       // current values
  var defaults = {};    // default values from server
  var debounceMs = 30;  // throttle for slider drag → OSC

  // ---------- DOM helpers ----------

  function $(id) { return document.getElementById(id); }

  function el(tag, className, text) {
    var e = document.createElement(tag);
    if (className) e.className = className;
    if (text !== undefined) e.textContent = text;
    return e;
  }

  // ---------- HTTP ----------

  function postJson(path, body) {
    return fetch(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body || {}),
    }).then(r => r.json());
  }

  function getJson(path) {
    return fetch(path).then(r => r.json());
  }

  // ---------- Slider control ----------

  function makeControl(cfg, valueRefHolder) {
    var wrap = el('div', 'control');
    var row = el('div', 'control-row');
    var label = el('div', 'control-label', cfg.label);
    var value = el('div', 'control-value');
    row.appendChild(label);
    row.appendChild(value);

    var input = document.createElement('input');
    input.type = 'range';
    input.min = cfg.min;
    input.max = cfg.max;
    input.step = cfg.step;

    function refreshDisplay(v) {
      value.textContent = cfg.fmt(v);
      var pct = ((v - cfg.min) / (cfg.max - cfg.min)) * 100;
      input.style.setProperty('--fill', pct + '%');
    }

    var lastSent = NaN;
    var sendTimer = null;

    function sendNow(v) {
      lastSent = v;
      postJson('/api/set', { name: cfg.name, value: v }).catch(function () {});
    }

    input.addEventListener('input', function () {
      var v = parseFloat(input.value);
      state[cfg.name] = v;
      refreshDisplay(v);
      if (sendTimer) clearTimeout(sendTimer);
      sendTimer = setTimeout(function () { sendNow(v); }, debounceMs);
    });

    input.addEventListener('change', function () {
      var v = parseFloat(input.value);
      sendNow(v);
    });

    valueRefHolder[cfg.name] = function (newVal) {
      input.value = newVal;
      state[cfg.name] = newVal;
      refreshDisplay(newVal);
    };

    wrap.appendChild(row);
    wrap.appendChild(input);
    return wrap;
  }

  // ---------- Mode (dropdown) control ----------

  function makeModeControl(cfg, valueRefHolder) {
    var wrap = el('div', 'control');
    var row = el('div', 'control-row');
    var label = el('div', 'control-label', cfg.label);
    row.appendChild(label);

    var select = document.createElement('select');
    select.className = 'mode-select';
    cfg.options.forEach(function (opt) {
      var o = document.createElement('option');
      o.value = String(opt.value);
      o.textContent = opt.text;
      select.appendChild(o);
    });

    select.addEventListener('change', function () {
      var v = parseFloat(select.value);
      state[cfg.name] = v;
      refreshVisibility();
      postJson('/api/set', { name: cfg.name, value: v }).catch(function () {});
    });

    valueRefHolder[cfg.name] = function (newVal) {
      var nv = parseFloat(newVal);
      state[cfg.name] = nv;
      // Snap to the closest defined option in case server has a stray value.
      var best = cfg.options[0].value, bestDiff = Infinity;
      cfg.options.forEach(function (opt) {
        var d = Math.abs(opt.value - nv);
        if (d < bestDiff) { bestDiff = d; best = opt.value; }
      });
      select.value = String(best);
    };

    wrap.appendChild(row);
    wrap.appendChild(select);
    return wrap;
  }

  // ---------- Sections ----------

  var setters = {};        // map paramName → function(value) to update slider/dropdown position
  var controlNodes = {};   // map paramName → wrapper element (for showWhen toggling)
  var controlConfigs = {}; // map paramName → cfg (for showWhen lookup)

  // Walk all controls; toggle wrap.hidden based on cfg.showWhen(state).
  // Called after any state mutation so dependent controls show/hide live.
  function refreshVisibility() {
    Object.keys(controlConfigs).forEach(function (name) {
      var cfg = controlConfigs[name];
      if (!cfg.showWhen) return;
      var wrap = controlNodes[name];
      if (!wrap) return;
      wrap.hidden = !cfg.showWhen(state);
    });
  }

  function renderSections() {
    var root = $('sections');
    root.innerHTML = '';
    SECTIONS.forEach(function (s) {
      var sec = el('div', 'section' + (s.open ? ' open' : ''));
      var head = el('div', 'section-head');
      var title = el('span', null, s.title);
      var chev = el('span', 'chev', '▾');
      head.appendChild(title);
      head.appendChild(chev);
      sec.appendChild(head);
      var body = el('div', 'section-body');
      s.controls.forEach(function (cfg) {
        var wrap = (cfg.type === 'mode')
          ? makeModeControl(cfg, setters)
          : makeControl(cfg, setters);
        controlNodes[cfg.name] = wrap;
        controlConfigs[cfg.name] = cfg;
        body.appendChild(wrap);
      });
      sec.appendChild(body);
      head.addEventListener('click', function () {
        sec.classList.toggle('open');
      });
      root.appendChild(sec);
    });
  }

  function applyState(values) {
    Object.keys(values).forEach(function (name) {
      if (setters[name]) {
        setters[name](values[name]);
      } else {
        state[name] = values[name];
      }
    });
    refreshVisibility();
  }

  // ---------- Toast ----------

  function toast(msg, ms) {
    var t = $('savedToast');
    t.textContent = msg;
    t.hidden = false;
    setTimeout(function () { t.hidden = true; }, ms || 1800);
  }

  // ---------- Save / Load / Reset ----------

  // Wire a click handler if the element exists. A missing element (e.g.
  // because the browser served a stale index.html) shouldn't break the
  // rest of the action wiring.
  function onClick(id, fn) {
    var el = $(id);
    if (el) el.addEventListener('click', fn);
  }

  function setupActions() {
    onClick('btnSave', function () {
      postJson('/api/save', {}).then(function (res) {
        if (res.ok) toast('saved → ' + res.filename);
      });
    });

    onClick('btnMakeDefault', function () {
      if (!confirm('Save current settings as the auto-loaded default for next startup?')) return;
      postJson('/api/save-default', {}).then(function (res) {
        if (res && res.ok) {
          toast('current state is now the default');
        } else {
          toast('save default failed — restart server.py?', 4000);
        }
      }).catch(function () {
        toast('save default failed — restart server.py?', 4000);
      });
    });

    onClick('btnReset', function () {
      if (!confirm('Reset all parameters to defaults?')) return;
      postJson('/api/reset', {}).then(function (res) {
        if (res.ok) {
          applyState(res.state);
          toast('reset to defaults');
        }
      });
    });

    onClick('btnFactoryReset', function () {
      if (!confirm('Factory reset?\n\nThis deletes presets/_default.json (your saved default) and resets all parameters to the SC patch\'s factory values. Saved presets are kept.')) return;
      postJson('/api/clear-default', {}).then(function () {
        return postJson('/api/reset', {});
      }).then(function (res) {
        if (res && res.ok) {
          applyState(res.state);
          toast('factory reset complete');
        } else {
          toast('factory reset failed', 4000);
        }
      }).catch(function () {
        toast('factory reset failed', 4000);
      });
    });

    onClick('btnLoad', function () {
      getJson('/api/presets').then(function (res) {
        renderPresetList(res.presets || []);
        $('loadOverlay').hidden = false;
      });
    });

    onClick('btnLoadCancel', function () {
      $('loadOverlay').hidden = true;
    });
  }

  function renderPresetList(presets) {
    var ul = $('presetList');
    ul.innerHTML = '';
    if (presets.length === 0) {
      var li = el('li', null, 'No saved presets yet.');
      ul.appendChild(li);
      return;
    }
    presets.forEach(function (p) {
      var li = el('li');
      var when = el('span', 'preset-when', p.saved_at || p.filename);
      var noteText = p.note ? p.note : p.filename;
      var note = el('span', 'preset-note', noteText);
      li.appendChild(when);
      li.appendChild(note);
      li.addEventListener('click', function () {
        postJson('/api/load', { filename: p.filename }).then(function (res) {
          if (res.ok) {
            applyState(res.state);
            toast('loaded ' + p.filename);
          }
          $('loadOverlay').hidden = true;
        });
      });
      ul.appendChild(li);
    });
  }

  // ---------- Boot ----------

  function init() {
    renderSections();
    setupActions();
    getJson('/api/state').then(function (res) {
      defaults = res.defaults || {};
      applyState(res.state || {});
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
