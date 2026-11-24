// The panel on the right: the numbers of whatever is selected (or of the
// scene when nothing is), and under them the outline of everything.

import { S, $, hooks, MATERIALS, clone, findItem, select, isSelected, noteText, parseNote, round } from './state.js';
import { ops, send, toast } from './net.js';
import { duplicate, repeat, remove, rename } from './build.js';
import { bindKey, audition } from './play.js';

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
    else if (k === 'class') node.className = v;
    else node.setAttribute(k, v);
  }
  node.append(...children);
  return node;
}

const tidy = (v) => Number(Number(v).toPrecision(4));

/** Dragging sideways on a label changes its number: multiplying for
 *  quantities that span orders of magnitude, adding `step` otherwise. */
function scrub(label, input, set, step) {
  label.classList.add('scrub');
  label.addEventListener('pointerdown', (down) => {
    if (down.target !== label) return;
    const start = Number(input.value !== '' ? input.value : input.placeholder);
    if (!Number.isFinite(start)) return;
    label.setPointerCapture(down.pointerId);
    let value = start;
    const move = (e) => {
      const dx = e.clientX - down.clientX;
      const fine = e.shiftKey ? 0.1 : 1;
      value = step ? tidy(start + Math.round(dx / 3) * step * fine) : tidy(start * Math.exp(dx * 0.01 * fine));
      input.value = value;
    };
    const up = () => {
      label.removeEventListener('pointermove', move);
      label.removeEventListener('pointerup', up);
      if (value !== start) set(value);
    };
    label.addEventListener('pointermove', move);
    label.addEventListener('pointerup', up);
  });
}

/** A number field. An empty box means "leave it to the default". `step`
 *  makes dragging the label add instead of multiply. */
function num(label, value, set, hint = '', step = 0) {
  const input = el('input', { type: 'number', step: 'any', value: value ?? '', placeholder: hint });
  input.addEventListener('change', () => set(input.value === '' ? undefined : Number(input.value)));
  const name = el('span', { title: 'Drag sideways to change (Shift for fine steps)' }, label);
  scrub(name, input, set, step);
  return el('label', {}, name, input);
}

function triple(label, value, set) {
  const boxes = [0, 1, 2].map((i) => {
    const input = el('input', { type: 'number', step: 'any', value: value?.[i] ?? 0 });
    input.addEventListener('change', () => {
      const next = [...(value || [0, 0, 0])];
      next[i] = Number(input.value);
      set(next);
    });
    return input;
  });
  return el('label', {}, label, el('div', { class: 'triple' }, ...boxes));
}

function choice(label, value, options, set) {
  const select = el('select', {}, ...options.map((o) => {
    const opt = el('option', { value: o }, o);
    if (o === value) opt.selected = true;
    return opt;
  }));
  select.addEventListener('change', () => set(select.value));
  return el('label', {}, label, select);
}

const row = (...buttons) => el('div', { class: 'row' }, ...buttons);
const button = (label, run, cls = '', title = '') => el('button', { class: cls, title, onclick: run }, label);

/** Send `item` with `patch` applied as an upsert; undefined removes a field. */
function update(kind, item, patch) {
  const next = clone(item);
  for (const [k, v] of Object.entries(patch)) {
    if (v === undefined) delete next[k];
    else next[k] = v;
  }
  ops([{ type: kind, ...next }]);
}

const open = new Map([['Outline', true]]);   // which folds are open, by title

/** A fold: a titled section that opens and closes, and remembers which. */
function fold(box, title, build, aside = '') {
  const d = el('details', {}, el('summary', {}, title, el('small', {}, aside)));
  if (open.get(title)) d.setAttribute('open', '');
  d.addEventListener('toggle', () => open.set(title, d.open));
  build(d);
  box.append(d);
  return d;
}

function header(box, kind, item) {
  const name = el('input', { type: 'text', class: 'name', value: item.name, title: 'Rename' });
  name.addEventListener('change', () => rename(kind, item.name, name.value));
  const head = el('div', { class: 'head' }, el('span', { class: 'chip' }, kind === 'exciter' ? 'play point' : kind === 'part' ? item.shape.kind : item.kind), name);
  if (kind === 'part' && item.shape.kind !== 'mass') head.append(el('button', { title: 'Hear this part', onclick: () => audition(item.name) }, '▶'));
  box.append(head);
}

/** Whether a part has a pitch worth tuning. */
const pitched = (part) => ['string', 'bar', 'membrane'].includes(part.shape.kind);

/** A pitch, typed as a note name or as hertz, with buttons to step it. */
function pitchField(box, hz, set) {
  const input = el('input', { type: 'text', value: noteText(hz), title: 'A note (E2, F#3, Bb4) or a frequency in hertz' });
  input.addEventListener('change', () => {
    const typed = input.value.trim();
    const want = parseNote(typed) ?? (Number(typed) > 0 ? Number(typed) : null);
    if (want) set(round(want, 1000));
    else { toast(`"${typed}" is not a pitch: try E2, F#3, Bb4 or 196`, true); input.value = noteText(hz); }
  });
  box.append(el('label', {}, 'Pitch', input));
  const step = (label, semis, title) => button(label, () => set(round(hz * Math.pow(2, semis / 12), 1000)), '', title);
  box.append(el('div', { class: 'steps group' },
    step('−8ve', -12, 'An octave down'), step('−', -1, 'A semitone down'), step('+', 1, 'A semitone up'), step('+8ve', 12, 'An octave up')));
}

function partFields(box, item, set) {
  const sh = item.shape;
  const setShape = (patch) => {
    const shape = { ...sh };
    for (const [k, v] of Object.entries(patch)) { if (v === undefined) delete shape[k]; else shape[k] = v; }
    set({ shape });
  };
  const info = S.doc.parts[S.doc.scene.parts.indexOf(item)];
  // What it is meant to sound at: its own lowest mode, before any tuning correction.
  const low = info.hz[0] / (item.tune ?? 1);
  if (sh.kind !== 'mass') {
    box.append(choice('Material', typeof item.material === 'string' ? item.material : 'steel', MATERIALS, (v) => set({ material: v })));
  }
  if (sh.kind === 'string') {
    if (sh.tension != null) box.append(num('Tension N', sh.tension, (v) => setShape({ tension: v })));
    else pitchField(box, sh.f0, (v) => setShape({ f0: v }));
    box.append(num('Length m', sh.length, (v) => setShape({ length: v })));
    box.append(num('Radius m', sh.radius, (v) => setShape({ radius: v }), '0.0005'));
  } else if (sh.kind === 'bar') {
    box.append(num('Length m', sh.length, (v) => setShape({ length: v })));
    box.append(num('Width m', sh.width, (v) => setShape({ width: v })));
    box.append(num('Thickness m', sh.thickness, (v) => setShape({ thickness: v })));
    box.append(choice('Ends', sh.ends || 'free', ['free', 'supported'], (v) => setShape({ ends: v })));
  } else if (sh.kind === 'plate') {
    box.append(num('Size x m', sh.size[0], (v) => setShape({ size: [v, sh.size[1]] })));
    box.append(num('Size z m', sh.size[1], (v) => setShape({ size: [sh.size[0], v] })));
    box.append(num('Thickness m', sh.thickness, (v) => setShape({ thickness: v })));
  } else if (sh.kind === 'membrane') {
    if (sh.tension != null) box.append(num('Tension N/m', sh.tension, (v) => setShape({ tension: v })));
    else pitchField(box, sh.f0, (v) => setShape({ f0: v }));
    box.append(num('Radius m', sh.radius, (v) => setShape({ radius: v })));
  } else {
    box.append(num('Mass kg', sh.mass, (v) => setShape({ mass: v })));
  }
  box.append(el('div', { class: 'note' }, `${info.modes} modes${low > 0 ? `, lowest ${low.toFixed(1)} Hz (${noteText(low)})` : ''}`));

  if (pitched(item)) {
    const off = item.tune ? `Tuned: corrected by ${((item.tune - 1) * 100).toFixed(2)}% for what is tied to it.` : 'Whatever is tied to it pulls its pitch off. Tune corrects that.';
    box.append(el('div', { class: 'note' }, off));
    const buttons = [button('Tune', () => { toast('tuning…'); send({ type: 'tune', parts: [item.name] }); })];
    if (item.tune) buttons.push(button('Clear tuning', () => set({ tune: undefined })));
    box.append(row(...buttons));
  }

  fold(box, 'Placement', (d) => {
    d.append(triple('Position', item.pos, (v) => set({ pos: v })));
    d.append(triple('Rotation °', item.rot, (v) => set({ rot: v })));
  });

  fold(box, 'Repeat', (d) => {
    const field = (value, title) => el('input', { type: 'number', step: 'any', value, title });
    const count = field(5, 'How many more');
    const gap = [field(0, 'x'), field(0, 'y'), field(0.0105, 'z')];
    const semis = field(sh.kind === 'plate' || sh.kind === 'mass' ? 0 : 5, 'Semitones between one copy and the next');
    d.append(el('label', {}, 'Copies', count));
    d.append(el('label', {}, 'Spacing m', el('div', { class: 'triple' }, ...gap)));
    if (sh.kind !== 'plate' && sh.kind !== 'mass') d.append(el('label', {}, 'Semitones', semis));
    d.append(el('div', { class: 'note' }, 'Each copy brings its joints, pickups and play points along.'));
    d.append(row(button('Make copies', () => repeat(item.name, Number(count.value), gap.map((g) => Number(g.value)), Number(semis.value)))));
  }, 'a row of these');

  if (sh.kind !== 'mass') {
    fold(box, 'Damping and detail', (d) => {
      d.append(num('Loss', item.loss, (v) => set({ loss: v }), 'material'));
      d.append(num('Damping 1/s', item.damping, (v) => set({ damping: v }), '1'));
      d.append(num('Max modes', item.modes, (v) => set({ modes: v }), '96', 4));
    });
  }
}

function jointFields(box, item, set) {
  const end = (p) => (p ? `${p.part} [${(p.at || []).join(', ')}]` : 'ground');
  box.append(el('div', { class: 'note' }, `${end(item.a)} → ${end(item.b)}. Drag either end in the view to move it.`));
  box.append(choice('Kind', item.kind, ['spring', 'contact'], (v) => set({ kind: v, cubic: undefined, exponent: undefined, gap: undefined, stiffness: v === 'contact' ? 1e8 : 1e4 })));
  box.append(num(item.kind === 'spring' ? 'Stiffness N/m' : 'Stiffness', item.stiffness, (v) => set({ stiffness: v }), item.kind === 'spring' ? '1e4' : '1e8'));
  box.append(num('Damping', item.damping, (v) => set({ damping: v }), '0'));
  box.append(num(item.kind === 'spring' ? 'Free play m' : 'Gap m', item.gap, (v) => set({ gap: v }), '0'));
  if (item.kind === 'spring') box.append(num('Cubic N/m³', item.cubic, (v) => set({ cubic: v }), '0'));
  else box.append(num('Exponent', item.exponent, (v) => set({ exponent: v }), '1.5', 0.05));
  if (item.kind === 'spring' && !item.gap) box.append(el('div', { class: 'note' }, 'Give it a little free play (say 0.0005) and it rattles.'));
}

const STRENGTH = {
  mallet: ['Speed m/s', '2'], impulse: ['Impulse N·s', '0.01'], pluck: ['Pull m', '0.003'], bow: ['Bow speed m/s', '0.2'],
};

function exciterFields(box, item, set) {
  const fire = (strength) => () => send(strength === undefined ? { type: 'excite', exciter: item.name } : { type: 'excite', exciter: item.name, strength });
  if (item.kind === 'bow') box.append(row(button('▶ Bow', fire()), button('Lift', fire(0))));
  else box.append(row(button('▶ Play it', fire())));
  box.append(el('div', { class: 'note' }, `On ${item.on.part} [${(item.on.at || []).join(', ')}]. Drag it in the view to move it.`));

  const waiting = S.capture === item.name;
  box.append(el('label', {}, 'Key', el('div', { class: 'pair' },
    el('span', { class: 'keycap' }, waiting ? '…' : item.key ? item.key.toUpperCase() : 'none'),
    button(waiting ? 'press a key' : 'Bind', () => { bindKey(item.name); renderInspector(); }),
    button('Clear', () => set({ key: undefined })),
  )));
  const learning = S.learn === item.name;
  box.append(el('label', {}, 'MIDI note', el('div', { class: 'pair' },
    el('span', { class: 'keycap' }, learning ? '…' : item.note ?? 'none'),
    button(learning ? 'play a note' : 'Learn', () => send({ type: 'learn', exciter: learning ? null : item.name })),
    button('Clear', () => set({ note: undefined })),
  )));
  if (!S.midi.length) box.append(el('div', { class: 'note' }, 'No MIDI device is connected.'));

  box.append(choice('Kind', item.kind, ['mallet', 'pluck', 'bow', 'impulse'], (v) => set({
    kind: v, strength: undefined, mass: undefined, stiffness: undefined, damping: undefined, time: undefined, pressure: undefined,
  })));
  const [label, hint] = STRENGTH[item.kind];
  box.append(num(label, item.strength, (v) => set({ strength: v }), hint));
  if (item.kind === 'mallet') {
    box.append(num('Mass kg', item.mass, (v) => set({ mass: v }), '0.02'));
    box.append(num('Stiffness', item.stiffness, (v) => set({ stiffness: v }), '1e8'));
    box.append(num('Damping', item.damping, (v) => set({ damping: v }), '0.3'));
  }
  if (item.kind === 'pluck') box.append(num('Pull time s', item.time, (v) => set({ time: v }), '0.02'));
  if (item.kind === 'bow') box.append(num('Pressure N', item.pressure, (v) => set({ pressure: v }), '0.3'));
}

function listenerFields(box, item, set) {
  if (item.kind === 'mic') box.append(triple('Position', item.pos, (v) => set({ pos: v })));
  else box.append(el('div', { class: 'note' }, `Pickup on ${item.on.part} [${item.on.at.join(', ')}]. Drag it in the view to move it.`));
  box.append(num('Gain dB', item.gain_db, (v) => set({ gain_db: v ?? 0 }), '0', 0.5));
  box.append(num('Pan', item.pan, (v) => set({ pan: v ?? 0 }), '0', 0.05));
}

/** The scene's own settings: shown when nothing is selected. */
function sceneFields(box) {
  const s = S.doc.scene;
  box.append(el('h2', {}, 'Scene'));
  const name = el('input', { type: 'text', value: s.name || '' });
  name.addEventListener('change', () => ops([{ type: 'name', name: name.value }]));
  box.append(el('label', {}, 'Name', name));
  box.append(el('div', { class: 'note' }, 'Pick a part on the left and drag in the view to draw it. Double-click anything to hear it. Keys with a play point play at any time.'));
  if (s.parts.some(pitched)) {
    box.append(row(button('Tune all', () => { toast('tuning…'); send({ type: 'tune' }); }, '', 'Correct every string, bar and membrane so it sounds at its pitch. Run it again after changing joints.')));
  }
  fold(box, 'Engine', (d) => {
    const settings = s.settings || {};
    const setSettings = (patch) => {
      const next = { ...settings };
      for (const [k, v] of Object.entries(patch)) { if (v === undefined) delete next[k]; else next[k] = v; }
      ops([{ type: 'settings', ...next }]);
    };
    d.append(choice('Solver', settings.solver || 'sav', ['sav', 'explicit'], (v) => setSettings({ solver: v })));
    d.append(num('Oversample', settings.oversample, (v) => setSettings({ oversample: v }), 'auto', 1));
    d.append(num('Top Hz', settings.max_hz, (v) => setSettings({ max_hz: v }), '16000'));
    const info = S.doc.info;
    d.append(el('div', { class: 'note' }, `${info.modes} modes at ${info.rate} Hz, ${info.oversample}x${info.audio ? '' : ' (no audio device)'}`));
  });
}

/** Everything in the scene, by kind; click to select. */
function outline(box) {
  const s = S.doc.scene;
  const count = s.parts.length + s.joints.length + s.exciters.length + s.listeners.length;
  fold(box, 'Outline', (d) => {
    const section = (title, kind, list, describe) => {
      if (!list.length) return;
      d.append(el('h3', {}, title));
      d.append(el('ul', {}, ...list.map((x) => el('li', {
        class: isSelected(kind, x.name) ? 'on' : '',
        onclick: (e) => { if (S.tool !== 'play') hooks.setTool?.('select'); select([{ kind, name: x.name }], e.shiftKey); },
      }, el('span', {}, x.name), el('small', {}, describe(x))))));
    };
    section('Parts', 'part', s.parts, (p) => {
      const hz = S.doc.parts[s.parts.indexOf(p)]?.hz[0] / (p.tune ?? 1);
      return `${p.shape.kind}${pitched(p) && hz > 0 ? ` · ${noteText(p.shape.f0 ?? hz)}` : ''}${p.tune ? ' · tuned' : ''}`;
    });
    section('Joints', 'joint', s.joints, (j) => `${j.kind} ${j.a.part}–${j.b?.part ?? 'ground'}`);
    section('Play points', 'exciter', s.exciters, (e) => `${e.kind} on ${e.on.part}${e.key ? ` · ${e.key.toUpperCase()}` : ''}${e.note != null ? ` · note ${e.note}` : ''}`);
    section('Listeners', 'listener', s.listeners, (l) => l.kind);
  }, `${count}`);
}

export function renderInspector() {
  const box = $('inspector');
  const scroll = box.scrollTop;
  box.replaceChildren();
  if (!S.doc) return;
  for (const w of S.doc.info.warnings) box.append(el('div', { class: 'warn' }, w));
  if (!S.doc.scene.listeners.length) box.append(el('div', { class: 'warn' }, 'Nothing is listening, so nothing can be heard: add a microphone or a pickup.'));
  const sel = S.sel[0];
  const item = findItem(sel);
  if (S.sel.length > 1) {
    box.append(el('h2', {}, `${S.sel.length} selected`));
    box.append(el('div', { class: 'note' }, 'Drag one of them, or the arrows, to move them together. Shift-click adds to or removes from the selection.'));
    box.append(row(button('Duplicate', duplicate), button('Delete', remove, 'danger')));
  } else if (item) {
    const set = (patch) => update(sel.kind, item, patch);
    header(box, sel.kind, item);
    ({ part: partFields, joint: jointFields, exciter: exciterFields, listener: listenerFields })[sel.kind](box, item, set);
    box.append(row(button('Duplicate', duplicate, '', 'Cmd/Ctrl+D'), button('Delete', remove, 'danger', 'Delete or Backspace')));
  } else sceneFields(box);
  outline(box);
  box.scrollTop = scroll;
}
hooks.inspector = renderInspector;
