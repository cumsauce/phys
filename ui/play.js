// Playing: the hand (strike, pluck, bow) under the mouse, play points under
// the computer keyboard and on the dock, and hearing a part after an edit.

import { S, $, hooks, clone, listOf, partByName, freshName, select, noteOf } from './state.js';
import { ops, send, toast } from './net.js';
import { hitAt, portFromHit, flash, lightCap } from './viewport.js';

// ---------------------------------------------------------------- the hand's settings

/** The Play panel's settings as the fields of a strike message. */
export function mallet(hand = S.hand) {
  if (hand === 'pluck') return { kind: 'pluck', pull: Number($('pull').value) / 1000 };
  if (hand === 'bow') {
    return { kind: 'bow', speed: Number($('bowspeed').value), pressure: Math.pow(10, Number($('pressure').value)) };
  }
  return {
    kind: 'strike',
    speed: Number($('speed').value),
    stiffness: Math.pow(10, Number($('hard').value)),
    mass: Math.pow(10, Number($('mass').value)),
  };
}

export function setHand(h) {
  S.hand = h;
  for (const b of $('hand').children) b.classList.toggle('on', b.dataset.hand === h);
  for (const box of document.querySelectorAll('#dock [data-for]')) box.style.display = box.dataset.for === h ? '' : 'none';
  hooks.hint?.();
}

export function labels() {
  $('speed_out').textContent = `${Number($('speed').value).toFixed(1)} m/s`;
  const h = Number($('hard').value);
  $('hard_out').textContent = h < 6.5 ? 'felt' : h < 7.5 ? 'rubber' : h < 8.5 ? 'wood' : h < 9.5 ? 'hard plastic' : 'steel';
  $('mass_out').textContent = `${(Math.pow(10, Number($('mass').value)) * 1000).toFixed(0)} g`;
  $('pull_out').textContent = `${Number($('pull').value).toFixed(1)} mm`;
  $('bowspeed_out').textContent = `${Number($('bowspeed').value).toFixed(2)} m/s`;
  $('pressure_out').textContent = `${Math.pow(10, Number($('pressure').value)).toFixed(2)} N`;
}

export function hush() {
  send({ type: 'hush' });
  held.clear();
}

// ---------------------------------------------------------------- the mouse

let press = null;   // the press under way in the Play tool

const strummable = (name) => ['string', 'bar'].includes(partByName(name)?.shape.kind);

/** A press in the Play tool: on a part, or beside one to start a strum. */
export function playDown(event, hit) {
  const port = portFromHit(hit);
  press = {
    port, point: hit?.point.clone() ?? null, moved: false, last: port?.part ?? null, sent: 0,
    trail: [{ x: event.clientX, y: event.clientY, t: performance.now() }],
  };
  if (S.hand === 'bow' && port) {
    // The bow sounds for as long as the button is held.
    send({ type: 'strike', ...port, ...mallet() });
    flash(hit.point, 0x5aa2e8);
  }
  return true;
}

export function playMove(event) {
  if (!press) return;
  const now = performance.now();
  press.trail.push({ x: event.clientX, y: event.clientY, t: now });
  if (press.trail.length > 12) press.trail.shift();
  const first = press.trail[0];
  if (Math.hypot(event.clientX - first.x, event.clientY - first.y) > 6) press.moved = true;
  if (S.hand === 'strike') return;
  if (S.hand === 'bow') {
    // Sliding along the part moves the bow.
    const port = portFromHit(hitAt(event, 'part'));
    if (press.port && port && port.part === press.port.part && now - press.sent > 40) {
      press.sent = now;
      press.port = port;
      send({ type: 'strike', ...port, ...mallet() });
    }
    return;
  }
  // Pluck: strum each string the cursor crosses. The mouse reports only
  // now and then, so walk the path since the last report a few pixels at
  // a time, or a quick strum would step over strings.
  const from = press.trail[press.trail.length - 2] ?? first;
  const steps = Math.max(1, Math.ceil(Math.hypot(event.clientX - from.x, event.clientY - from.y) / 1.5));
  for (let i = 1; i <= steps; i++) {
    const at = { clientX: from.x + (event.clientX - from.x) * i / steps, clientY: from.y + (event.clientY - from.y) * i / steps };
    const hit = hitAt(at, 'part');
    const port = portFromHit(hit);
    const name = port?.part ?? null;
    if (name === press.last) continue;
    press.last = name;
    if (name && strummable(name)) {
      send({ type: 'strike', ...port, ...mallet() });
      flash(hit.point);
    }
  }
}

export function playUp(event) {
  const p = press;
  press = null;
  if (!p) return;
  if (S.hand === 'bow') return send({ type: 'bow_stop' });
  if (S.hand === 'pluck') {
    if (!p.moved && p.port) { send({ type: 'strike', ...p.port, ...mallet() }); flash(p.point); }
    return;
  }
  // Strike: a flick sets the strength by its speed; a plain click uses the slider.
  const settings = mallet();
  let port = p.port, point = p.point;
  if (p.moved) {
    const now = performance.now();
    const from = p.trail.find((s) => now - s.t < 90) ?? p.trail[0];
    const last = p.trail[p.trail.length - 1];
    const dt = Math.max((last.t - from.t) / 1000, 0.01);
    const pixels = Math.hypot(last.x - from.x, last.y - from.y) / dt;
    settings.speed = Math.min(10, Math.max(0.3, pixels / 300));
    const hit = hitAt(event, 'part');
    if (portFromHit(hit)) { port = portFromHit(hit); point = hit.point; }
    if (port) toast(`struck at ${settings.speed.toFixed(1)} m/s`);
  }
  if (!port) return;
  send({ type: 'strike', ...port, ...settings });
  flash(point);
}

/** The scroll wheel changes the pressure of a held bow. */
export function playWheel(event) {
  if (!press?.port || S.hand !== 'bow') return false;
  const slider = $('pressure');
  slider.value = Number(slider.value) - Math.sign(event.deltaY) * 0.04;
  labels();
  send({ type: 'strike', ...press.port, ...mallet() });
  return true;
}

export const pressing = () => press !== null;

/** Play one point with the hand as it is set, from any tool. A bow cannot
 *  be held this way, so it plucks a string and strikes anything else. */
export function playAt(port, point) {
  const hand = S.hand !== 'bow' ? S.hand : partByName(port.part)?.shape.kind === 'string' ? 'pluck' : 'strike';
  send({ type: 'strike', ...port, ...mallet(hand) });
  if (point) flash(point);
}
hooks.playAt = playAt;

/** Where and how a part is sounded to hear what an edit did to it. */
const AUDITION = {
  string: { kind: 'pluck', at: [0.2], pull: 0.003 },
  bar: { kind: 'strike', at: [0.5], speed: 2, stiffness: 1e8, mass: 0.02 },
  plate: { kind: 'strike', at: [0.37, 0.41], speed: 2, stiffness: 1e8, mass: 0.02 },
  membrane: { kind: 'strike', at: [0.4, 0.45], speed: 2, stiffness: 1e7, mass: 0.02 },
};

/** Sound a part once, without adding to the take. */
export function audition(name) {
  const how = AUDITION[partByName(name)?.shape.kind];
  if (how) send({ type: 'audition', part: name, ...how });
}

// ---------------------------------------------------------------- play points

/** A play point at `port` with the hand's current settings. */
function playPoint(port, taken) {
  const m = mallet();
  const base = { name: freshName(`play_${port.part}`, listOf('exciter'), taken), on: port };
  if (m.kind === 'pluck') return { ...base, kind: 'pluck', strength: m.pull };
  if (m.kind === 'bow') return { ...base, kind: 'bow', pressure: Number(m.pressure.toFixed(3)), strength: m.speed };
  return { ...base, kind: 'mallet', mass: Number(m.mass.toPrecision(3)), stiffness: Number(m.stiffness.toPrecision(3)), strength: m.speed };
}

export function addPlayPoint(port) {
  const e = playPoint(port, new Set());
  select([{ kind: 'exciter', name: e.name }]);
  ops([{ type: 'exciter', ...e }]);
  bindKey(e.name);
}
hooks.addPlayPoint = addPlayPoint;

/** Wait for the next key press and bind it to a play point. */
export function bindKey(name) {
  S.capture = name;
  toast(`press the key that should play ${name} (with Shift to share the key with what it already plays; Esc to skip)`);
}

const ROWS = 'asdfghjkl;qwertyuiopzxcvbnm,./';

/** Give each selected part (or every string and bar) a play point and a
 *  key, left to right along a keyboard row. */
export function assignKeys() {
  let parts = S.sel.filter((s) => s.kind === 'part').map((s) => partByName(s.name)).filter(Boolean);
  if (!parts.length) parts = listOf('part').filter((p) => ['string', 'bar'].includes(p.shape.kind));
  if (!parts.length) parts = listOf('part').filter((p) => p.shape.kind !== 'mass');
  if (!parts.length) return toast('there is nothing to play yet', true);
  // Order them along whichever way they are spread out most.
  const spread = (i) => Math.max(...parts.map((p) => p.pos?.[i] ?? 0)) - Math.min(...parts.map((p) => p.pos?.[i] ?? 0));
  const axis = spread(2) >= spread(0) ? 2 : 0;
  parts = [...parts].sort((a, b) => (a.pos?.[axis] ?? 0) - (b.pos?.[axis] ?? 0));
  const mine = new Set(parts.map((p) => p.name));
  const used = new Set(listOf('exciter').filter((e) => e.key && !mine.has(e.on.part)).map((e) => e.key));
  const keys = [...ROWS].filter((k) => !used.has(k));
  const taken = new Set();
  const batch = [];
  parts.forEach((part, i) => {
    const key = keys[i];
    if (!key) return;
    const have = listOf('exciter').find((e) => e.on.part === part.name);
    if (have) batch.push({ type: 'exciter', ...clone(have), key });
    else {
      const at = { string: [0.2], bar: [0.2], plate: [0.3, 0.4], membrane: [0.4, 0.45], mass: [] }[part.shape.kind];
      batch.push({ type: 'exciter', ...playPoint({ part: part.name, at }, taken), key });
    }
  });
  ops(batch);
  toast(`keys ${keys.slice(0, Math.min(parts.length, keys.length)).join(' ').toUpperCase()} now play ${parts.length} parts`);
}

// ---------------------------------------------------------------- the keyboard

const CODES = {
  Semicolon: ';', Comma: ',', Period: '.', Slash: '/', Quote: "'", BracketLeft: '[', BracketRight: ']', Minus: '-', Equal: '=',
};

/** The character a physical key stands for, whatever Shift or Alt make of it. */
function keyChar(code) {
  if (code.startsWith('Key')) return code.slice(3).toLowerCase();
  if (code.startsWith('Digit')) return code.slice(5);
  return CODES[code] ?? null;
}

const held = new Map();   // key code -> the bows it is holding down and the dampers it is holding up
/** Bows and dampers follow the key: they end when it comes up. */
const follows = (e) => e.kind === 'bow' || e.kind === 'damper';
let pending = null;       // a key chosen for a play point the scene does not have yet

/** Bind the chosen key once its play point exists. */
export function applyPending() {
  if (!pending || !listOf('exciter').some((e) => e.name === pending.name)) return;
  const { name, key, share } = pending;
  pending = null;
  const batch = [];
  // A key moves to its new play point unless it is shared: with Shift (the
  // strings of one note), or with a damper, which belongs under a hammer's key.
  const mine = listOf('exciter').find((e) => e.name === name);
  for (const e of listOf('exciter')) {
    if (e.name === name) batch.push({ type: 'exciter', ...clone(e), key });
    else if (e.key === key && !share && mine.kind !== 'damper' && e.kind !== 'damper') {
      const c = clone(e); delete c.key; batch.push({ type: 'exciter', ...c });
    }
  }
  ops(batch);
  toast(`${key.toUpperCase()} plays ${name}`);
}

/** Returns true when the key was used for playing. */
export function keyDown(event) {
  const ch = keyChar(event.code);
  if (S.capture) {
    const name = S.capture;
    if (event.key === 'Escape') { S.capture = null; toast(''); return true; }
    if (!ch) return true;
    S.capture = null;
    // The play point may still be on its way back from the backend: the
    // binding waits for it.
    pending = { name, key: ch, share: event.shiftKey };
    applyPending();
    return true;
  }
  if (event.code === 'Space') { hush(); return true; }
  if (!ch) return false;
  const targets = listOf('exciter').filter((e) => e.key === ch);
  if (!targets.length) return false;
  if (event.repeat) return true;
  const scale = event.shiftKey ? 1.6 : event.altKey ? 0.5 : 1;
  // Dampers lift before the hammers land.
  for (const e of [...targets.filter((e) => e.kind === 'damper'), ...targets.filter((e) => e.kind !== 'damper')]) {
    send({ type: 'excite', exciter: e.name, scale });
    lightCap(e.name);
  }
  const bows = targets.filter(follows).map((e) => e.name);
  if (bows.length) held.set(event.code, bows);
  return true;
}

export function keyUp(event) {
  const bows = held.get(event.code);
  if (!bows) return;
  held.delete(event.code);
  for (const name of bows) send({ type: 'excite', exciter: name, strength: 0 });
}

// ---------------------------------------------------------------- the keys on the dock

/** The play points that have a key, as caps that can be pressed with the mouse. */
export function renderKeys() {
  const box = $('keys');
  box.replaceChildren();
  const order = (e) => { const i = ROWS.indexOf(e.key); return i < 0 ? 99 : i; };
  const bound = listOf('exciter').filter((e) => e.key).sort((a, b) => order(a) - order(b));
  if (!bound.length) {
    const none = document.createElement('span');
    none.className = 'none';
    none.textContent = 'No keys yet';
    box.append(none);
    return;
  }
  // One cap for each key: it plays everything bound to that key, as the key does.
  const groups = new Map();
  for (const e of bound) groups.set(e.key, [...(groups.get(e.key) ?? []), e]);
  for (const [key, all] of groups) {
    // Dampers lift before the hammers land.
    const group = [...all.filter((e) => e.kind === 'damper'), ...all.filter((e) => e.kind !== 'damper')];
    const e = group.find((x) => x.kind !== 'damper') ?? group[0];
    const cap = document.createElement('button');
    cap.dataset.exciters = JSON.stringify(group.map((x) => x.name));
    cap.title = group.map((x) => `${x.name}: ${x.kind} on ${x.on.part}`).join('\n');
    const part = partByName(e.on.part);
    const info = S.doc.parts[S.doc.scene.parts.indexOf(part)];
    const note = part && part.shape.kind !== 'plate' && part.shape.kind !== 'mass' ? noteOf(part.shape.f0 ?? info?.hz[0] / (part.tune ?? 1)) : null;
    const small = document.createElement('small');
    small.textContent = note?.name ?? e.on.part.slice(0, 5);
    cap.append(key.toUpperCase(), small);
    cap.addEventListener('pointerdown', (ev) => {
      const scale = ev.shiftKey ? 1.6 : ev.altKey ? 0.5 : 1;
      for (const x of group) {
        send({ type: 'excite', exciter: x.name, scale });
        lightCap(x.name);
      }
      const ending = group.filter(follows);
      if (ending.length) {
        window.addEventListener('pointerup', () => {
          for (const x of ending) send({ type: 'excite', exciter: x.name, strength: 0 });
        }, { once: true });
      }
    });
    box.append(cap);
  }
}

hooks.keyLit = (name) => {
  for (const cap of $('keys').children) {
    if (!cap.dataset?.exciters || !JSON.parse(cap.dataset.exciters).includes(name)) continue;
    cap.classList.add('lit');
    setTimeout(() => cap.classList.remove('lit'), 160);
  }
};
