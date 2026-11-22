// phys editor. The backend owns the scene and the physics; this page draws
// it, sends edits as ops, and plays it.
//
//   state.js      what the page knows
//   net.js        the connection
//   viewport.js   the 3D view, picking, the move gizmo
//   handles.js    resize handles and dragging attachments
//   build.js      add, duplicate, repeat, delete, rename, right-click menu
//   play.js       strike, pluck, bow; play points and keys
//   inspector.js  the panel on the right

import { S, $, hooks, listOf, freshName, partByName, select, pruneSelection } from './state.js';
import { connect, send, ops, toast, settle } from './net.js';
import {
  canvas, orbit, gizmo, gizmoState, world, overlay, buildWorld, applySelection, hover, hitAt, portFromHit, flash, tip, tick,
  frame, lookFrom, setSnapping, snapshot, lightCap, camera, THREE,
} from './viewport.js';
import { handleDown, portDragStart, dragging, dragMove, dragUp, dragCancel, snapPort } from './handles.js';
import { startPlacing, stopPlacing, placeAt, duplicate, remove, contextMenu } from './build.js';
import { setHand, labels as handLabels, mallet, playDown, playMove, playUp, playWheel, pressing, keyDown, keyUp, hush, assignKeys, applyPending } from './play.js';
import { renderInspector } from './inspector.js';

// ---------------------------------------------------------------- tools

function setTool(t) {
  S.tool = t;
  S.jointFrom = null;
  stopPlacing();
  $('view').className = t;
  $('strikebox').style.display = t === 'strike' ? '' : 'none';
  // In the Play tool the left button plays, so the right one turns the view.
  orbit.mouseButtons = t === 'strike'
    ? { LEFT: null, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.ROTATE }
    : { LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN };
  applySelection();
}
hooks.setTool = setTool;

function syncButtons() {
  for (const b of $('tools').children) b.classList.toggle('on', b.dataset.tool === S.tool);
  for (const b of $('gizmo').children) b.classList.toggle('on', b.dataset.mode === gizmo.mode);
  $('undo').disabled = !S.doc?.undo;
  $('redo').disabled = !S.doc?.redo;
}
hooks.buttons = syncButtons;
hooks.selection = () => { applySelection(); renderInspector(); };
hooks.rebuild = () => { if (S.doc) buildWorld(); };

// ---------------------------------------------------------------- the mouse

let down = null;      // the press under way: where, and what it landed on

// Runs before the camera's own handler, so a press that starts a drag of
// ours can switch the camera off in time.
canvas.addEventListener('pointerdown', (e) => {
  gizmoState.used = false;
  if (e.button !== 0 || !S.doc) { down = null; return; }
  const hit = gizmo.axis ? null : hitAt(e);
  down = { x: e.clientX, y: e.clientY, hit };
  if (gizmo.axis || S.placing) return;
  if (S.tool === 'strike') {
    playDown(e, hit);
    orbit.enabled = false;
  } else if (S.tool === 'select') {
    if (handleDown(e) || hit?.pick.end) orbit.enabled = false;
  }
}, { capture: true });

canvas.addEventListener('pointermove', (e) => {
  if (dragging()) return dragMove(e);
  if (pressing()) return playMove(e);
  const far = down && Math.hypot(e.clientX - down.x, e.clientY - down.y) > 4;
  if (far && S.tool === 'select' && down.hit?.pick.end && !gizmoState.used && e.buttons === 1) {
    // A press on an attachment that then moves is a drag of that attachment.
    if (portDragStart(down.hit)) return dragMove(e);
  }
  if (!e.buttons) hover(e);
});

window.addEventListener('pointerup', (e) => {
  const press = down;
  down = null;
  orbit.enabled = true;
  if (dragging()) return dragUp();
  if (pressing()) return playUp(e);
  if (!press || e.button !== 0 || gizmoState.used || !S.doc || e.target !== canvas) return;
  if (Math.hypot(e.clientX - press.x, e.clientY - press.y) > 4) return;
  click(e, press.hit);
});
window.addEventListener('blur', () => { if (pressing()) playUp({ clientX: 0, clientY: 0 }); });

function click(e, hit) {
  if (S.placing) return placeAt(e);
  const raw = portFromHit(hit);
  const port = raw ? { part: raw.part, at: snapPort(partByName(raw.part).shape, raw.at).at } : null;
  if (S.tool === 'joint') {
    if (!S.jointFrom) {
      if (!port) return toast('click a part to start the joint', true);
      S.jointFrom = port;
      flash(hit.point, 0x6fbf73);
      return toast('now click the second point (empty space ties it to the ground)');
    }
    const joint = { type: 'joint', name: freshName('joint', listOf('joint')), kind: 'spring', a: S.jointFrom, stiffness: 1e4 };
    if (port) joint.b = port;
    S.jointFrom = null;
    S.sel = [{ kind: 'joint', name: joint.name }];
    return ops([joint]);
  }
  if (S.tool === 'pickup') {
    if (!port) return;
    const name = freshName('pickup', listOf('listener'));
    S.sel = [{ kind: 'listener', name }];
    return ops([{ type: 'listener', name, kind: 'pickup', on: port }]);
  }
  if (S.tool === 'select') {
    if (hit) select([hit.pick], e.shiftKey);
    else if (!e.shiftKey) select([]);
  }
}

canvas.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  if (S.doc) contextMenu(e);
});
canvas.addEventListener('wheel', (e) => {
  if (playWheel(e)) { e.preventDefault(); e.stopImmediatePropagation(); }
}, { capture: true, passive: false });
canvas.addEventListener('pointerleave', () => { if (!dragging()) tip(''); });

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
  if (e.key === 'Delete' || e.key === 'Backspace') remove();
  else if (e.key === 'Escape') {
    S.jointFrom = null;
    stopPlacing();
    dragCancel();
    select([]);
  } else if (key === 's') setTool('select');
  else if (key === 'h') setTool('strike');
  else if (key === 'j') setTool('joint');
  else if (key === 'p') setTool('pickup');
  else if (key === 'w') { gizmo.setMode('translate'); syncButtons(); }
  else if (key === 'e') { gizmo.setMode('rotate'); syncButtons(); }
  else if (key === 'f') frame();
  else if (key === '1') lookFrom('front');
  else if (key === '3') lookFrom('side');
  else if (key === '7') lookFrom('top');
});
window.addEventListener('keyup', (e) => {
  if (e.key === 'Alt') { S.alt = false; setSnapping(); }
  keyUp(e);
});

// ---------------------------------------------------------------- messages from the backend

function showFile() {
  const doc = S.doc;
  const file = doc.path ? doc.path.split('/').pop() : 'not saved';
  $('file').textContent = `${doc.scene.name || 'untitled'}${doc.dirty ? ' *' : ''} — ${file}`;
  $('file').title = doc.path || 'This scene has not been saved to a file yet';
  document.title = `phys — ${$('file').textContent}`;
}

function showMidi() {
  $('midi').textContent = S.midi.length ? `MIDI: ${S.midi.join(', ')}` : 'MIDI: none';
  $('midi').className = S.midi.length ? '' : 'dim';
}

const takeLabel = (n) => { $('clear').textContent = n ? `Clear take (${n})` : 'Clear take'; };

connect({
  scene(msg) {
    S.doc = msg;
    S.amps = [];
    S.midi = msg.midi ?? [];
    S.learn = msg.learn ?? null;
    pruneSelection();
    buildWorld();
    renderInspector();
    settle();
    $('stats').textContent = `${msg.info.modes} modes`;
    takeLabel(msg.take);
    showFile();
    showMidi();
    applyPending();
  },
  tm(msg) {
    S.amps = msg.amps;
    const db = 20 * Math.log10(Math.max(msg.peak, 1e-6));
    const level = $('level');
    level.style.width = `${Math.max(0, Math.min(100, (db + 60) / 60 * 100))}%`;
    level.className = db > -1 ? 'hot' : '';
    $('load').textContent = `load ${(msg.load * 100).toFixed(0)}%${msg.blown ? ' — blew up, reset' : ''}`;
    $('load').className = msg.load > 0.8 || msg.blown ? 'bad' : '';
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
    if (!S.sel.length) renderInspector();
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

for (const b of $('tools').children) b.addEventListener('click', () => setTool(b.dataset.tool));
for (const b of $('gizmo').children) b.addEventListener('click', () => { gizmo.setMode(b.dataset.mode); syncButtons(); });
for (const b of $('hand').children) b.addEventListener('click', () => setHand(b.dataset.hand));
$('add').addEventListener('change', (e) => { if (e.target.value) startPlacing(e.target.value); e.target.value = ''; });
$('undo').addEventListener('click', () => send({ type: 'undo' }));
$('redo').addEventListener('click', () => send({ type: 'redo' }));
$('save').addEventListener('click', () => send({ type: 'save' }));
$('saveas').addEventListener('click', () => send({ type: 'save_as' }));
$('open').addEventListener('click', () => send({ type: 'open' }));
$('new').addEventListener('click', () => send({ type: 'new' }));
$('render').addEventListener('click', () => { toast('rendering…'); send({ type: 'render' }); });
$('clear').addEventListener('click', () => send({ type: 'clear_take' }));
$('volume').addEventListener('input', (e) => send({ type: 'volume', value: Number(e.target.value) }));
$('hush').addEventListener('click', hush);
$('assign').addEventListener('click', assignKeys);
for (const id of ['speed', 'hard', 'mass', 'pull', 'bowspeed', 'pressure']) $(id).addEventListener('input', handLabels);
handLabels();
setHand('strike');
setTool('select');

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
  S, send, ops, world, overlay, camera, hitAt, setTool, setHand, select, mallet, assignKeys, duplicate, remove,
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
