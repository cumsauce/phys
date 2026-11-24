// phys editor. The backend owns the scene and the physics; this page draws
// it, sends edits as ops, and plays it.
//
//   state.js      what the page knows
//   net.js        the connection
//   viewport.js   the 3D view, picking, the move gizmo
//   handles.js    resize handles, dragging parts and attachments
//   draw.js       drawing new parts and joints in the view
//   build.js      duplicate, repeat, delete, rename, right-click menu
//   play.js       strike, pluck, bow; play points and keys; auditions
//   inspector.js  the panel on the right
//
// Keys: digits choose tools, letters belong to the instrument (a letter
// bound to a play point plays it in any tool), Tab flips between building
// and playing.

import { S, $, hooks, prefs, setPref, isDraw, listOf, freshName, partByName, select, isSelected, pruneSelection, noteOf } from './state.js';
import { connect, send, ops, toast } from './net.js';
import {
  canvas, orbit, gizmo, gizmoState, world, overlay, buildWorld, applySelection, hover, hitAt, portFromHit, flash, tip, tick,
  frame, lookFrom, setSnapping, snapshot, lightCap, camera, THREE,
} from './viewport.js';
import { handleDown, portDragStart, moveDragStart, dragging, dragMove, dragUp, dragCancel, snapPort } from './handles.js';
import { drawDown, drawMove, drawUp, drawCancel, drawing, jointDown, jointMove, jointUp, jointing, makeJoint } from './draw.js';
import { duplicate, remove, contextMenu } from './build.js';
import {
  setHand, labels as handLabels, mallet, playDown, playMove, playUp, playWheel, pressing, playAt, keyDown, keyUp, hush, assignKeys,
  applyPending, audition, renderKeys,
} from './play.js';
import { renderInspector } from './inspector.js';

// ---------------------------------------------------------------- tools

const svg = (d) => `<svg viewBox="0 0 20 20">${d}</svg>`;
const TOOLS = [
  { id: 'select', label: 'Select', key: '1', icon: svg('<path d="M5 3l10 7-4.5 1L13 16l-2 1-2.5-5L5 15z"/>'), title: 'Select, move and resize' },
  { id: 'play', label: 'Play', key: '2', icon: svg('<circle cx="14" cy="6" r="3"/><path d="M12 8L4 16"/>'), title: 'Strike, pluck or bow by hand (Tab flips here and back)' },
  null,
  { id: 'string', label: 'String', key: '3', icon: svg('<path d="M3 10c4-3 10 3 14 0"/><circle cx="3" cy="10" r="1"/><circle cx="17" cy="10" r="1"/>'), title: 'Draw a string' },
  { id: 'bar', label: 'Bar', key: '4', icon: svg('<rect x="2.5" y="7.5" width="15" height="5" rx="1"/>'), title: 'Draw a bar' },
  { id: 'plate', label: 'Plate', key: '5', icon: svg('<path d="M6 6h12l-4 8H2z"/>'), title: 'Draw a plate' },
  { id: 'membrane', label: 'Skin', key: '6', icon: svg('<ellipse cx="10" cy="10" rx="7.5" ry="4.5"/><ellipse cx="10" cy="10" rx="3" ry="1.6"/>'), title: 'Draw a membrane (a drum skin)' },
  { id: 'mass', label: 'Mass', key: '7', icon: svg('<circle cx="10" cy="10" r="4" fill="currentColor"/>'), title: 'Drop a point mass' },
  null,
  { id: 'joint', label: 'Joint', key: '8', icon: svg('<path d="M3 10h2l1.5-4 3 8 3-8 1.5 4h3"/>'), title: 'Tie two points together with a spring' },
  { id: 'pickup', label: 'Pickup', key: '9', icon: svg('<rect x="4" y="9" width="7" height="5" rx="1"/><path d="M11 11.5c4 0 5-2 5-7"/>'), title: 'Stick a contact pickup on a part' },
  { id: 'mic', label: 'Mic', key: '0', icon: svg('<rect x="7.5" y="2.5" width="5" height="9" rx="2.5"/><path d="M5 9.5a5 5 0 0010 0M10 14.5v3"/>'), title: 'Place a microphone' },
];

for (const t of TOOLS) {
  if (!t) { $('rail').append(document.createElement('hr')); continue; }
  const b = document.createElement('button');
  b.dataset.tool = t.id;
  b.title = `${t.title} (${t.key})`;
  b.innerHTML = `${t.icon}<span>${t.label}</span><kbd>${t.key}</kbd>`;
  b.addEventListener('click', () => setTool(t.id));
  $('rail').append(b);
}

const HAND_HINT = {
  strike: '<b>Click</b> to strike, or <b>flick</b> across a point to hit as hard as you sweep',
  pluck: '<b>Click</b> to pluck, or <b>drag across</b> strings to strum',
  bow: '<b>Hold</b> on a part to bow it · slide to move the bow · scroll to press harder',
};
const TIE = '<label><input type="checkbox" id="autotie"> tie it to what it lands on</label>';
const HINTS = {
  select: () => (S.sel.length
    ? '<b>Drag</b> it to slide it · white handles resize · <b>double-click</b> to hear it'
    : '<b>Click</b> to select · <b>double-click</b> to hear · right-click for more'),
  play: () => `${HAND_HINT[S.hand]} · right-drag turns the view`,
  string: () => `<b>Drag</b> from one end to the other, or click for a default string${TIE}`,
  bar: () => `<b>Drag</b> from one end to the other, or click for a default bar${TIE}`,
  plate: () => '<b>Drag</b> from corner to corner, or click for a default plate',
  membrane: () => '<b>Drag</b> out from the centre, or click for a default membrane',
  mass: () => `<b>Click</b> where the mass should go${TIE}`,
  joint: () => (S.jointFrom
    ? 'Now <b>click</b> the second point (empty space ties it to the ground)'
    : '<b>Drag</b> from one point to another, or click both · end on empty space to tie to the ground'),
  pickup: () => '<b>Click</b> a part to stick a pickup there',
  mic: () => '<b>Click</b> where the microphone should go',
};

function showHint() {
  $('hint').innerHTML = HINTS[S.tool]() + (isDraw(S.tool) ? ' · Shift keeps the tool' : '');
  const tie = $('autotie');
  if (tie) {
    tie.checked = prefs.autotie;
    tie.addEventListener('change', () => setPref('autotie', tie.checked));
  }
}
hooks.hint = showHint;

function setTool(t) {
  if (t === 'play' && S.tool !== 'play') S.back = S.tool;
  S.tool = t;
  S.jointFrom = null;
  drawCancel();
  $('view').className = isDraw(t) ? 'draw' : t;
  // Where the left button draws or plays, the right one turns the view.
  orbit.mouseButtons = t === 'play' || isDraw(t)
    ? { LEFT: null, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.ROTATE }
    : { LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN };
  applySelection();
  showHint();
}
hooks.setTool = setTool;

function syncButtons() {
  for (const b of $('rail').children) b.classList.toggle('on', b.dataset.tool === S.tool);
  for (const b of $('gizmo').children) b.classList.toggle('on', b.dataset.mode === gizmo.mode);
  $('gizmo').style.display = S.tool === 'select' && S.sel.some((s) => s.kind === 'part') ? '' : 'none';
  $('undo').disabled = !S.doc?.undo;
  $('redo').disabled = !S.doc?.redo;
}
hooks.buttons = syncButtons;
hooks.selection = () => { applySelection(); renderInspector(); showHint(); };
hooks.rebuild = () => { if (S.doc) buildWorld(); };
hooks.prefs = (key) => { if (key === 'notes') hooks.rebuild(); };

function toggleGizmo(mode = gizmo.mode === 'translate' ? 'rotate' : 'translate') {
  gizmo.setMode(mode);
  syncButtons();
}

// ---------------------------------------------------------------- the mouse

let down = null;      // the left press under way: where, and what it landed on
let right = null;     // where the right button went down

// Runs before the camera's own handler, so a press that starts a drag of
// ours can switch the camera off in time.
canvas.addEventListener('pointerdown', (e) => {
  gizmoState.used = false;
  if (e.button === 2) right = { x: e.clientX, y: e.clientY };
  if (e.button !== 0 || !S.doc) { down = null; return; }
  const hit = gizmo.axis ? null : hitAt(e);
  down = { x: e.clientX, y: e.clientY, hit };
  if (gizmo.axis) return;
  if (isDraw(S.tool)) {
    if (drawDown(e)) orbit.enabled = false;
  } else if (S.tool === 'play') {
    playDown(e, hit);
    orbit.enabled = false;
  } else if (S.tool === 'joint') {
    if (!S.jointFrom && jointDown(e, hit)) orbit.enabled = false;
  } else if (S.tool === 'select') {
    const mine = hit && (hit.pick.end || (hit.pick.kind === 'part' && isSelected('part', hit.pick.name)));
    if (handleDown(e) || mine) orbit.enabled = false;
  }
}, { capture: true });

canvas.addEventListener('pointermove', (e) => {
  if (dragging()) return dragMove(e);
  if (drawing()) return drawMove(e);
  if (jointing()) return jointMove(e);
  if (pressing()) return playMove(e);
  const far = down && Math.hypot(e.clientX - down.x, e.clientY - down.y) > 4;
  if (far && S.tool === 'select' && down.hit && !gizmoState.used && e.buttons === 1) {
    // A press on an attachment that then moves drags that attachment; one
    // on a selected part slides the selection over the ground.
    const pick = down.hit.pick;
    if (pick.end ? portDragStart(down.hit) : pick.kind === 'part' && isSelected('part', pick.name) && moveDragStart(down.hit)) return dragMove(e);
  }
  if (!e.buttons) hover(e);
});

window.addEventListener('pointerup', (e) => {
  if (e.button === 2) {
    // A right-click that did not turn the view opens the menu.
    const r = right;
    right = null;
    if (r && S.doc && e.target === canvas && Math.hypot(e.clientX - r.x, e.clientY - r.y) < 4) contextMenu(e);
    return;
  }
  const press = down;
  down = null;
  orbit.enabled = true;
  if (dragging()) return dragUp();
  if (drawing()) return drawUp(e);
  if (jointing() && jointUp(e)) return;
  if (pressing()) return playUp(e);
  if (!press || e.button !== 0 || gizmoState.used || !S.doc || e.target !== canvas) return;
  if (Math.hypot(e.clientX - press.x, e.clientY - press.y) > 4) return;
  click(e, press.hit);
});
window.addEventListener('blur', () => {
  if (pressing()) playUp({ clientX: 0, clientY: 0 });
  drawCancel();
});

function click(e, hit) {
  const raw = portFromHit(hit);
  const port = raw ? { part: raw.part, at: snapPort(partByName(raw.part).shape, raw.at).at } : null;
  if (S.tool === 'joint') {
    if (!S.jointFrom) {
      if (!port) return toast('click a part to start the joint', true);
      S.jointFrom = port;
      flash(hit.point, 0x6fbf73);
      return showHint();
    }
    const from = S.jointFrom;
    S.jointFrom = null;
    showHint();
    return makeJoint(from, port);
  }
  if (S.tool === 'pickup') {
    if (!port) return toast('click a part to put the pickup on', true);
    const name = freshName('pickup', listOf('listener'));
    S.sel = [{ kind: 'listener', name }];
    return ops([{ type: 'listener', name, kind: 'pickup', on: port }]);
  }
  if (S.tool === 'select') {
    if (hit) select([hit.pick], e.shiftKey);
    else if (!e.shiftKey) select([]);
  }
}

// A double-click plays the point, whatever the tool (the Play tool has
// already played it twice).
canvas.addEventListener('dblclick', (e) => {
  if (!S.doc || S.tool === 'play' || isDraw(S.tool)) return;
  const hit = hitAt(e, 'part');
  const port = portFromHit(hit);
  if (port) playAt(port, hit.point);
});

canvas.addEventListener('contextmenu', (e) => e.preventDefault());
canvas.addEventListener('wheel', (e) => {
  if (playWheel(e)) { e.preventDefault(); e.stopImmediatePropagation(); }
}, { capture: true, passive: false });
canvas.addEventListener('pointerleave', () => { if (!dragging() && !drawing()) tip(''); });

// ---------------------------------------------------------------- the keyboard

function typing(e) {
  return e.target.matches?.('input, select, textarea');
}

window.addEventListener('keydown', (e) => {
  if (e.key === 'Alt') { S.alt = true; setSnapping(); }
  if (typing(e)) return;
  const mod = e.metaKey || e.ctrlKey;
  const key = e.key.toLowerCase();
  if (mod) {
    if (key === 'z') send({ type: e.shiftKey ? 'redo' : 'undo' });
    else if (key === 's') send({ type: 'save' });
    else if (key === 'd') duplicate();
    else return;
    return e.preventDefault();
  }
  if (keyDown(e)) return e.preventDefault();
  const tool = TOOLS.find((t) => t && e.code === `Digit${t.key}`);
  if (tool) setTool(tool.id);
  else if (e.key === 'Delete' || e.key === 'Backspace') remove();
  else if (e.key === 'Escape') {
    closePop();
    if (drawing() || jointing() || dragging() || S.jointFrom) {
      S.jointFrom = null;
      drawCancel();
      dragCancel();
      showHint();
    } else if (S.tool !== 'select') setTool('select');
    else select([]);
  } else if (e.key === 'Tab') {
    setTool(S.tool === 'play' ? S.back : 'play');
    e.preventDefault();
  } else if (e.code === 'Backquote') toggleGizmo();
  else if (e.code === 'Backslash' || e.key === 'Home') frame();
});
window.addEventListener('keyup', (e) => {
  if (e.key === 'Alt') { S.alt = false; setSnapping(); }
  keyUp(e);
});

// ---------------------------------------------------------------- menus on the top bar

function closePop() {
  $('pop').style.display = 'none';
}
window.addEventListener('pointerdown', (e) => {
  if (!$('pop').contains(e.target) && !e.target.closest?.('#filebtn, #viewbtn')) closePop();
});

/** A menu under a button. Items: [label, run, hint] or [label, run, hint, checked] to stay open as a toggle, or null for a line. */
function popMenu(anchor, items) {
  const pop = $('pop');
  if (pop.style.display === 'block' && pop.dataset.for === anchor.id) return closePop();
  pop.dataset.for = anchor.id;
  const fill = () => {
    pop.replaceChildren();
    for (const item of items()) {
      if (!item) { pop.append(document.createElement('hr')); continue; }
      const [label, run, hint = '', checked] = item;
      const b = document.createElement('button');
      const left = document.createElement('span');
      if (checked !== undefined) {
        const tick = document.createElement('span');
        tick.className = 'check';
        tick.textContent = checked ? '✓' : '';
        left.append(tick);
      }
      left.append(label);
      const right = document.createElement('small');
      right.textContent = hint;
      b.append(left, right);
      b.addEventListener('click', () => {
        run();
        if (checked === undefined) closePop(); else fill();
      });
      pop.append(b);
    }
  };
  fill();
  const r = anchor.getBoundingClientRect();
  pop.style.display = 'block';
  pop.style.left = `${r.left}px`;
  pop.style.top = `${r.bottom + 4}px`;
}

$('filebtn').addEventListener('click', () => popMenu($('filebtn'), () => [
  ['New', () => send({ type: 'new' }), ''],
  ['Open…', () => send({ type: 'open' }), ''],
  null,
  ['Save', () => send({ type: 'save' }), '⌘S'],
  ['Save As…', () => send({ type: 'save_as' }), ''],
]));

$('viewbtn').addEventListener('click', () => popMenu($('viewbtn'), () => [
  ['Show vibration', () => setPref('vibes', !prefs.vibes), '', prefs.vibes],
  ['Note names on parts', () => setPref('notes', !prefs.notes), '', prefs.notes],
  ['Hear each edit', () => setPref('hear', !prefs.hear), '', prefs.hear],
]));

// ---------------------------------------------------------------- messages from the backend

function showFile() {
  const doc = S.doc;
  const file = doc.path ? doc.path.split('/').pop() : 'not saved';
  $('file').textContent = `${doc.scene.name || 'untitled'}${doc.dirty ? ' •' : ''}`;
  $('path').textContent = file;
  $('path').title = doc.path || 'This scene has not been saved to a file yet';
  document.title = `phys — ${doc.scene.name || 'untitled'}${doc.dirty ? ' *' : ''} — ${file}`;
}

function showMidi() {
  $('midi').textContent = S.midi.length ? `MIDI: ${S.midi.join(', ')}` : 'MIDI: none';
  $('midi').className = S.midi.length ? '' : 'dim';
}

let pitchAt = 0;
/** The tuner: the pitch of the part last played. It holds for a moment, then dims. */
function showPitch(hz) {
  const now = performance.now();
  const n = noteOf(hz);
  if (n) {
    pitchAt = now;
    const cents = n.cents === 0 ? '±0' : `${n.cents > 0 ? '+' : '−'}${Math.abs(n.cents)}`;
    $('tuner_note').textContent = n.name;
    $('tuner_hz').textContent = `${cents}c · ${hz.toFixed(1)} Hz`;
    $('tuner_needle').style.left = `${50 + n.cents}%`;
    $('tuner').className = Math.abs(n.cents) <= 3 ? 'good' : '';
  } else if (now - pitchAt > 2500) {
    $('tuner').className = 'dim';
  }
}

function takeLabel(n) {
  $('render').textContent = n ? `Render take (${n})` : 'Render take';
  $('render').disabled = !n;
  $('clear').disabled = !n;
}

/** What makes a part sound the way it does: a change here is worth hearing. */
const voice = (p) => JSON.stringify([p.shape, p.material, p.loss, p.damping, p.modes]);

/** After an edit made here, sound the parts it changed: one at once, a few as a run. */
function hearEdit(before, after) {
  if (!prefs.hear || !before || performance.now() - S.sentAt > 1500) return;
  const old = new Map(before.parts.map((p) => [p.name, voice(p)]));
  const changed = after.parts.filter((p) => old.get(p.name) !== voice(p) && p.shape.kind !== 'mass');
  if (!changed.length || changed.length > 16) return;
  changed.forEach((p, i) => setTimeout(() => audition(p.name), i * 110));
}

connect({
  scene(msg) {
    const before = S.doc?.scene;
    S.doc = msg;
    S.amps = [];
    S.midi = msg.midi ?? [];
    S.learn = msg.learn ?? null;
    pruneSelection();
    buildWorld();
    renderInspector();
    renderKeys();
    showHint();
    $('stats').textContent = `${msg.info.modes} modes`;
    takeLabel(msg.take);
    showFile();
    showMidi();
    applyPending();
    hearEdit(before, msg.scene);
  },
  tm(msg) {
    S.amps = msg.amps;
    const db = 20 * Math.log10(Math.max(msg.peak, 1e-6));
    const level = $('level');
    level.style.width = `${Math.max(0, Math.min(100, (db + 60) / 60 * 100))}%`;
    level.className = db > -1 ? 'hot' : '';
    $('load').textContent = `load ${(msg.load * 100).toFixed(0)}%${msg.blown ? ' — blew up, reset' : ''}`;
    $('load').className = msg.load > 0.8 || msg.blown ? 'bad' : '';
    showPitch(msg.pitch);
  },
  take: (msg) => takeLabel(msg.count),
  played(msg) {
    takeLabel(msg.take);
    if (msg.on) lightCap(msg.exciter);
  },
  midi(msg) { S.midi = msg.devices; showMidi(); renderInspector(); },
  midi_note() {
    $('midi').classList.add('lit');
    setTimeout(() => $('midi').classList.remove('lit'), 120);
  },
  learn(msg) { S.learn = msg.exciter; renderInspector(); },
  note: (msg) => toast(msg.message),
  error(msg) {
    toast(msg.message, true);
    hooks.rebuild();        // put anything a failed drag left behind back
    renderInspector();      // and rejected values
  },
  file(msg) {
    S.doc.path = msg.path;
    S.doc.dirty = msg.dirty;
    showFile();
  },
  ask(msg) {
    // In a browser there is no file dialog: ask here, then send the action
    // again with what was missing.
    if (msg.dirty && !confirm('The scene has changes that are not saved. Discard them?')) return;
    if (msg.what === 'new') return send({ type: 'new', force: true });
    const path = prompt(msg.what === 'open' ? 'Scene file to open:' : 'Save the scene as:', msg.path || 'phys-scene.json');
    if (path) send({ type: msg.what, path, force: true });
  },
  shot: () => send({ type: 'shot', data: snapshot() }),
});

// ---------------------------------------------------------------- wiring

for (const b of $('gizmo').children) b.addEventListener('click', () => toggleGizmo(b.dataset.mode));
for (const b of $('hand').children) b.addEventListener('click', () => setHand(b.dataset.hand));
for (const b of $('viewbtns').children) b.addEventListener('click', () => (b.dataset.view === 'fit' ? frame() : lookFrom(b.dataset.view)));
$('undo').addEventListener('click', () => send({ type: 'undo' }));
$('redo').addEventListener('click', () => send({ type: 'redo' }));
$('render').addEventListener('click', () => { toast('rendering…'); send({ type: 'render' }); });
$('clear').addEventListener('click', () => send({ type: 'clear_take' }));
$('volume').addEventListener('input', (e) => send({ type: 'volume', value: Number(e.target.value) }));
$('hush').addEventListener('click', hush);
$('assign').addEventListener('click', assignKeys);
for (const id of ['speed', 'hard', 'mass', 'pull', 'bowspeed', 'pressure']) $(id).addEventListener('input', handLabels);
// A slider that keeps the focus would swallow the keys that play.
document.addEventListener('pointerup', (e) => { if (e.target.matches?.('input[type="range"], button')) e.target.blur(); });
handLabels();
setHand('strike');
setTool('select');
takeLabel(0);

let last = performance.now();
function loop(now) {
  const dt = Math.min(0.1, (now - last) / 1000);
  last = now;
  tick(dt, now);
  requestAnimationFrame(loop);
}
requestAnimationFrame(loop);

// For driving the page from a console or a test.
window.phys = {
  S, send, ops, world, overlay, camera, hitAt, setTool, setHand, select, mallet, assignKeys, duplicate, remove, audition, prefs, setPref,
  step: (dt) => tick(dt, performance.now()),
  /** Where a world object is on screen, in CSS pixels. */
  screen(object) {
    const r = canvas.getBoundingClientRect();
    const v = object.getWorldPosition(new THREE.Vector3()).project(camera);
    return [(v.x + 1) / 2 * r.width + r.left, (1 - v.y) / 2 * r.height + r.top];
  },
  get doc() { return S.doc; },
  get amps() { return S.amps; },
};
