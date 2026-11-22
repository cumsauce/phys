// Making things: adding, placing, duplicating, repeating, renaming and
// deleting, and the right-click menu.

import { S, $, hooks, round, clone, listOf, findItem, partByName, freshName, select } from './state.js';
import { ops, toast } from './net.js';
import { THREE, hitAt, portFromHit, groundAt, portWorld, worldToPort, attachments, applySelection } from './viewport.js';

const TEMPLATES = {
  plate: { shape: { kind: 'plate', size: [0.4, 0.3], thickness: 0.003 }, material: 'wood' },
  bar: { shape: { kind: 'bar', length: 0.3, width: 0.04, thickness: 0.012 }, material: 'aluminium' },
  string: { shape: { kind: 'string', length: 0.65, radius: 0.0004, f0: 196 }, material: 'steel' },
  membrane: { shape: { kind: 'membrane', radius: 0.15, f0: 120 }, material: 'mylar' },
  mass: { shape: { kind: 'mass', mass: 0.01 } },
};

// ---------------------------------------------------------------- adding by clicking

export function startPlacing(what) {
  S.placing = what;
  $('view').classList.add('placing');
  applySelection();   // the gizmo steps aside while placing
  toast(`click where the ${what === 'mic' ? 'microphone' : what} should go (Esc to cancel)`);
}

export function stopPlacing() {
  const was = S.placing;
  S.placing = null;
  $('view').classList.remove('placing');
  if (was) applySelection();
}

/** Put the thing being placed where the click landed: just above a part, or on the ground. */
export function placeAt(event) {
  const what = S.placing;
  const hit = hitAt(event, 'part');
  const point = hit ? hit.point.clone() : groundAt(event);
  if (!point) return toast('click on the ground or on a part', true);
  const lift = what === 'mic' ? 0.25 : hit ? 0.02 : 0.05;
  point.y += lift;
  const pos = point.toArray().map((x) => round(x, 100));
  stopPlacing();
  if (what === 'mic') {
    const name = freshName('mic', listOf('listener'));
    select([{ kind: 'listener', name }]);
    return ops([{ type: 'listener', name, kind: 'mic', pos }]);
  }
  const name = freshName(what, listOf('part'));
  select([{ kind: 'part', name }]);
  ops([{ type: 'part', name, pos, ...clone(TEMPLATES[what]) }]);
  toast('');
}

// ---------------------------------------------------------------- copies

/** A part's pitch moved by `semis` semitones, where its shape has one. */
function transpose(shape, semis) {
  if (!semis) return;
  const ratio = Math.pow(2, semis / 12);
  if (shape.kind === 'string' || shape.kind === 'membrane') {
    if (shape.f0 != null) shape.f0 = round(shape.f0 * ratio, 1000);
    else if (shape.tension != null) shape.tension = round(shape.tension * ratio * ratio, 1000);
  } else if (shape.kind === 'bar') {
    shape.length = round(shape.length / Math.sqrt(ratio), 1e5);   // a bar's pitch goes as 1 / length^2
  }
}

/** The ops that copy `partName` `count` times at `spacing` apart, each
 *  `semis` semitones above the last, with everything attached to it. An
 *  attachment to another part lands on that part at the shifted point. */
function copyOps(partName, count, spacing, semis, taken) {
  const part = partByName(partName);
  if (!part) return { batch: [], names: [] };
  const step = new THREE.Vector3(...spacing);
  const att = attachments(partName);
  const batch = [], names = [];
  for (let k = 1; k <= count; k++) {
    const name = freshName(partName, listOf('part'), taken.part);
    names.push(name);
    const copy = clone(part);
    copy.name = name;
    copy.pos = (part.pos || [0, 0, 0]).map((x, i) => round(x + spacing[i] * k, 1e5));
    transpose(copy.shape, semis * k);
    batch.push({ type: 'part', ...copy });
    const shift = step.clone().multiplyScalar(k);
    const move = (port) => {
      if (!port) return port;
      if (port.part === partName) return { part: name, at: port.at };
      // The far end slides along its own part by the same step.
      return worldToPort(port.part, portWorld(port).add(shift)) ?? port;
    };
    for (const j of att.joints) {
      const c = clone(j);
      c.name = freshName(j.name, listOf('joint'), taken.joint);
      c.a = move(j.a);
      if (j.b) c.b = move(j.b);
      batch.push({ type: 'joint', ...c });
    }
    for (const l of att.listeners) {
      const c = clone(l);
      c.name = freshName(l.name, listOf('listener'), taken.listener);
      c.on = move(l.on);
      batch.push({ type: 'listener', ...c });
    }
    for (const e of att.exciters) {
      const c = clone(e);
      c.name = freshName(e.name, listOf('exciter'), taken.exciter);
      c.on = move(e.on);
      delete c.key;    // a key or a note plays one thing: the copy gets its own later
      delete c.note;
      batch.push({ type: 'exciter', ...c });
    }
  }
  return { batch, names };
}

const newTaken = () => ({ part: new Set(), joint: new Set(), listener: new Set(), exciter: new Set() });

/** Copy the selection a little to one side. */
export function duplicate() {
  const taken = newTaken();
  const batch = [], next = [];
  for (const s of S.sel) {
    if (s.kind === 'part') {
      const r = copyOps(s.name, 1, [0, 0, 0.03], 0, taken);
      batch.push(...r.batch);
      next.push(...r.names.map((name) => ({ kind: 'part', name })));
    } else {
      const item = clone(findItem(s));
      if (!item) continue;
      item.name = freshName(item.name, listOf(s.kind), taken[s.kind]);
      if (item.pos) item.pos = item.pos.map((x, i) => round(x + (i === 2 ? 0.05 : 0), 1e4));
      if (s.kind === 'exciter') { delete item.key; delete item.note; }
      batch.push({ type: s.kind, ...item });
      next.push({ kind: s.kind, name: item.name });
    }
  }
  if (!batch.length) return;
  S.sel = next;
  ops(batch);
}

/** `count` more of a part, `spacing` apart, `semis` semitones apart. */
export function repeat(partName, count, spacing, semis) {
  if (!(count >= 1 && count <= 64)) return toast('give 1 to 64 copies', true);
  const r = copyOps(partName, Math.round(count), spacing, semis, newTaken());
  S.sel = [{ kind: 'part', name: partName }, ...r.names.map((name) => ({ kind: 'part', name }))];
  ops(r.batch);
}

// ---------------------------------------------------------------- deleting and renaming

/** Delete the selection, and with a part everything attached to it. */
export function remove() {
  const gone = { part: new Set(), joint: new Set(), exciter: new Set(), listener: new Set() };
  for (const s of S.sel) gone[s.kind].add(s.name);
  for (const name of gone.part) {
    const att = attachments(name);
    att.joints.forEach((j) => gone.joint.add(j.name));
    att.exciters.forEach((e) => gone.exciter.add(e.name));
    att.listeners.forEach((l) => gone.listener.add(l.name));
  }
  const batch = [];
  const score = S.doc.scene.score || [];
  const kept = score.filter((ev) => !gone.exciter.has(ev.exciter));
  if (kept.length !== score.length) batch.push({ type: 'score', events: kept });
  // Attachments go before the parts they hang on.
  for (const kind of ['joint', 'exciter', 'listener', 'part']) {
    for (const name of gone[kind]) batch.push({ type: 'delete', kind, name });
  }
  S.sel = [];
  ops(batch);
}

export function rename(kind, from, to) {
  to = to.trim();
  if (!to || to === from) return hooks.inspector?.();
  for (const s of S.sel) if (s.kind === kind && s.name === from) s.name = to;
  ops([{ type: 'rename', kind, from, to }]);
}

// ---------------------------------------------------------------- the right-click menu

function closeMenu() {
  $('menu').style.display = 'none';
}
window.addEventListener('pointerdown', (e) => { if (!$('menu').contains(e.target)) closeMenu(); });
window.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeMenu(); });

export function contextMenu(event) {
  const hit = hitAt(event);
  const menu = $('menu');
  menu.replaceChildren();
  const add = (label, run, cls = '') => {
    const b = document.createElement('button');
    b.textContent = label;
    b.className = cls;
    b.addEventListener('click', () => { closeMenu(); run(); });
    menu.append(b);
  };
  const port = portFromHit(hit);
  if (hit) {
    const pick = hit.pick;
    if (!S.sel.some((s) => s.kind === pick.kind && s.name === pick.name)) select([pick]);
    const title = document.createElement('div');
    title.className = 'title';
    title.textContent = S.sel.length > 1 ? `${S.sel.length} selected` : pick.name;
    menu.append(title);
    if (port) {
      add('Add play point here', () => hooks.addPlayPoint?.(port));
      add('Put a pickup here', () => {
        const name = freshName('pickup', listOf('listener'));
        select([{ kind: 'listener', name }]);
        ops([{ type: 'listener', name, kind: 'pickup', on: port }]);
      });
      add('Start a joint here', () => {
        hooks.setTool?.('joint');
        S.jointFrom = port;
        toast('now click the second point (empty space ties it to the ground)');
      });
    }
    add('Duplicate', duplicate);
    if (S.sel.length === 1) {
      add('Rename…', () => {
        hooks.setTool?.('select');
        hooks.inspector?.();
        const field = document.querySelector('#inspector input.name');
        field?.focus();
        field?.select();
      });
    }
    add('Delete', remove, 'danger');
  } else {
    for (const what of ['plate', 'bar', 'string', 'membrane', 'mass', 'mic']) {
      add(`Add ${what === 'mic' ? 'microphone' : what} here`, () => { S.placing = what; placeAt(event); });
    }
  }
  const rect = $('view').getBoundingClientRect();
  menu.style.display = 'block';
  menu.style.left = `${Math.min(event.clientX - rect.left, rect.width - 190)}px`;
  menu.style.top = `${Math.min(event.clientY - rect.top, rect.height - menu.offsetHeight - 6)}px`;
}
