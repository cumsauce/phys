// Drawing: press and drag in the view to make a part the size and the way
// round you drew it. A string or bar runs from the press to the release, a
// plate spans the two corners, a membrane grows from its centre. A plain
// click makes a default one there. What is drawn over another part is
// tied to it, so it can be heard through it straight away.

import { S, hooks, prefs, deg, round, clone, listOf, partByName, freshName } from './state.js';
import { ops, toast } from './net.js';
import { THREE, overlay, hitAt, portFromHit, groundAt, mouseRay, partGroup, portAt, tip, clearGroup, flash } from './viewport.js';
import { snapPort } from './handles.js';

export const TEMPLATES = {
  plate: { shape: { kind: 'plate', size: [0.4, 0.3], thickness: 0.003 }, material: 'wood' },
  bar: { shape: { kind: 'bar', length: 0.3, width: 0.04, thickness: 0.012 }, material: 'aluminium' },
  string: { shape: { kind: 'string', length: 0.65, radius: 0.0004, f0: 196 }, material: 'steel' },
  membrane: { shape: { kind: 'membrane', radius: 0.15, f0: 120 }, material: 'mylar' },
  mass: { shape: { kind: 'mass', mass: 0.01 } },
};

/** How far above where it was drawn a new thing sits: [on a part, on the ground]. */
const LIFT = { string: [0.012, 0.05], bar: [0.02, 0.05], plate: [0.03, 0.05], membrane: [0.03, 0.05], mass: [0.02, 0.05], mic: [0.25, 0.25] };

/** Where along itself a new string or bar is tied down, and how stiffly. A
 *  bar rests on the nodes of its lowest mode, so the supports barely damp it. */
const TIES = { string: { at: [0.02, 0.98], stiffness: 2e4 }, bar: { at: [0.224, 0.776], stiffness: 5e3 } };

const UP = new THREE.Vector3(0, 1, 0);
const X = new THREE.Vector3(1, 0, 0);

// ---------------------------------------------------------------- geometry

/** The point under the cursor: on a part, else on the level plane at height `level`. */
function anchor(event, level = 0, parts = true) {
  const hit = parts ? hitAt(event, 'part') : null;
  if (hit) return { point: hit.point.clone(), on: hit.pick.name };
  const p = new THREE.Vector3();
  const plane = new THREE.Plane(UP, -level);
  return mouseRay(event).intersectPlane(plane, p) && p.length() < 20 ? { point: p, on: null } : null;
}

/** The port on `name` under a point in the world, or null when the point is not over that part. */
function portUnder(name, point) {
  const part = partByName(name);
  const group = partGroup(name);
  if (!part || !group) return null;
  const p = group.worldToLocal(point.clone());
  const sh = part.shape;
  const inside = {
    plate: () => Math.abs(p.x) <= sh.size[0] / 2 && Math.abs(p.z) <= sh.size[1] / 2,
    membrane: () => Math.hypot(p.x, p.z) <= sh.radius,
    bar: () => Math.abs(p.x) <= sh.length / 2 && Math.abs(p.z) <= sh.width / 2,
    string: () => Math.abs(p.x) <= sh.length / 2 && Math.abs(p.z) <= 0.01,
    mass: () => true,
  }[sh.kind]();
  return inside ? { part: name, at: portAt(sh, p) } : null;
}

/** The turn (degrees about X, then Y, then Z) that lays a part's own X axis along `d`. */
function rotAlong(d) {
  const yaw = Math.asin(Math.min(1, Math.max(-1, -d.z)));
  return [0, round(deg(yaw), 100), round(deg(Math.atan2(d.y, d.x)), 100)];
}

const pos = (v) => v.toArray().map((x) => round(x, 1e4));

// ---------------------------------------------------------------- making things

/** A string or bar from `a` to `b` ({point, on}), with its ties. */
function lineOps(kind, a, b) {
  if (b.point.x < a.point.x) [a, b] = [b, a];    // the part's X axis runs toward +X
  const lift = LIFT[kind][a.on || b.on ? 0 : 1];
  const p0 = a.point.clone().addScaledVector(UP, lift), p1 = b.point.clone().addScaledVector(UP, lift);
  const d = p1.clone().sub(p0);
  const length = d.length();
  d.normalize();
  const name = freshName(kind, listOf('part'));
  const part = { type: 'part', name, ...clone(TEMPLATES[kind]), pos: pos(p0.clone().lerp(p1, 0.5)), rot: rotAlong(d) };
  part.shape.length = round(length, 1e4);
  const batch = [part];
  if (prefs.autotie) {
    const taken = new Set();
    TIES[kind].at.forEach((u, i) => {
      const point = p0.clone().addScaledVector(d, u * length);
      const under = [i ? b.on : a.on, i ? a.on : b.on].filter(Boolean).map((n) => portUnder(n, point)).find(Boolean);
      if (!under) return;
      batch.push({
        type: 'joint', name: freshName(`tie_${name}`, listOf('joint'), taken), kind: 'spring',
        a: { part: name, at: [u] }, b: under, stiffness: TIES[kind].stiffness,
      });
    });
  }
  return { name, batch };
}

function make(tool, a, b) {
  const lift = LIFT[tool][a.on ? 0 : 1];
  const up = a.point.clone().addScaledVector(UP, lift);
  if (tool === 'mic') {
    const name = freshName('mic', listOf('listener'));
    return { kind: 'listener', name, batch: [{ type: 'listener', name, kind: 'mic', pos: pos(up) }] };
  }
  if (tool === 'string' || tool === 'bar') {
    if (!b) {
      // A click: a default one, centred there, lying along X.
      const half = new THREE.Vector3(TEMPLATES[tool].shape.length / 2, 0, 0);
      b = { point: a.point.clone().add(half), on: a.on };
      a = { point: a.point.clone().sub(half), on: a.on };
    }
    return { kind: 'part', ...lineOps(tool, a, b) };
  }
  const name = freshName(tool, listOf('part'));
  const part = { type: 'part', name, ...clone(TEMPLATES[tool]), pos: pos(up) };
  const batch = [part];
  if (tool === 'plate' && b) {
    part.shape.size = [round(Math.abs(b.point.x - a.point.x), 1e4), round(Math.abs(b.point.z - a.point.z), 1e4)];
    part.pos = pos(a.point.clone().lerp(b.point, 0.5).addScaledVector(UP, lift));
  } else if (tool === 'membrane' && b) {
    part.shape.radius = round(a.point.distanceTo(b.point), 1e4);
  } else if (tool === 'mass' && a.on && prefs.autotie) {
    const under = portUnder(a.on, a.point);
    if (under) batch.push({ type: 'joint', name: freshName(`tie_${name}`, listOf('joint')), kind: 'spring', a: under, b: { part: name, at: [] }, stiffness: 1e4 });
  }
  return { kind: 'part', name, batch };
}

function commit(tool, a, b) {
  const made = make(tool, a, b);
  S.sel = [{ kind: made.kind, name: made.name }];
  ops(made.batch);
  const ties = made.batch.length - 1;
  if (ties) toast(`${made.name}: tied to ${[...new Set(made.batch.slice(1).map((j) => (j.a.part === made.name ? j.b.part : j.a.part)))].join(' and ')}`);
}

/** A default `what` where the event points (the right-click menu's "Add … here"). */
export function placeDefault(what, event) {
  const a = anchor(event);
  if (!a) return toast('point at the ground or at a part', true);
  commit(what, a, null);
}

// ---------------------------------------------------------------- the drag

let d = null;       // the draw under way
const ghost = new THREE.Group();
overlay.add(ghost);
const ghostMaterial = () => new THREE.MeshBasicMaterial({ color: 0xe8a33d, transparent: true, opacity: 0.6, depthTest: false });

export function drawDown(event) {
  const a = anchor(event);
  if (!a) { toast('press on the ground or on a part', true); return false; }
  d = { tool: S.tool, a, b: null, x: event.clientX, y: event.clientY };
  return true;
}

/** Whether the drag has grown big enough to be a shape rather than a click. */
function sized() {
  if (!d?.b) return false;
  const dx = Math.abs(d.b.point.x - d.a.point.x), dz = Math.abs(d.b.point.z - d.a.point.z);
  if (d.tool === 'plate') return dx >= 0.03 && dz >= 0.03;
  if (d.tool === 'membrane') return d.a.point.distanceTo(d.b.point) >= 0.03;
  return d.a.point.distanceTo(d.b.point) >= 0.05;
}

export function drawMove(event) {
  if (!d || d.tool === 'mass' || d.tool === 'mic') return;
  if (!d.b && Math.hypot(event.clientX - d.x, event.clientY - d.y) < 5) return;
  const line = d.tool === 'string' || d.tool === 'bar';
  const b = anchor(event, d.a.point.y, line);
  if (!b) return;
  const a = d.a.point;
  if (!S.alt && !b.on) {
    // Whole centimetres; a line also turns in steps of 15 degrees.
    const v = b.point.clone().sub(a);
    if (line || d.tool === 'membrane') {
      const length = Math.round(v.length() * 100) / 100;
      const step = Math.PI / 12;
      const angle = line ? Math.round(Math.atan2(v.z, v.x) / step) * step : Math.atan2(v.z, v.x);
      b.point.set(a.x + length * Math.cos(angle), a.y, a.z + length * Math.sin(angle));
    } else {
      b.point.set(a.x + Math.round(v.x * 100) / 100, a.y, a.z + Math.round(v.z * 100) / 100);
    }
  }
  if (!S.alt && b.on && line) {
    // Onto a part: the end stays where it was put, but for a whole centimetre.
    const v = b.point.clone().sub(a);
    const length = Math.round(v.length() * 100) / 100;
    if (length > 0) b.point.copy(a).addScaledVector(v.normalize(), length);
  }
  d.b = b;
  clearGroup(ghost);
  const mm = (x) => (x * 1000).toFixed(0);
  let mesh, text;
  if (line) {
    const v = b.point.clone().sub(a);
    const t = d.tool === 'bar' ? TEMPLATES.bar.shape : { thickness: 0.003, width: 0.003 };
    mesh = new THREE.Mesh(new THREE.BoxGeometry(Math.max(v.length(), 1e-4), t.thickness, t.width), ghostMaterial());
    mesh.position.copy(a).lerp(b.point, 0.5);
    mesh.quaternion.setFromUnitVectors(X, v.clone().normalize());
    text = `${d.tool} ${mm(v.length())} mm${b.on ? ` · onto ${b.on}` : ''}`;
  } else if (d.tool === 'plate') {
    const w = Math.abs(b.point.x - a.x), h = Math.abs(b.point.z - a.z);
    mesh = new THREE.Mesh(new THREE.BoxGeometry(Math.max(w, 1e-4), 0.003, Math.max(h, 1e-4)), ghostMaterial());
    mesh.position.copy(a).lerp(b.point, 0.5);
    text = `plate ${mm(w)} × ${mm(h)} mm`;
  } else {
    const r = a.distanceTo(b.point);
    mesh = new THREE.Mesh(new THREE.CylinderGeometry(r, r, 0.002, 48), ghostMaterial());
    mesh.position.copy(a);
    text = `membrane, radius ${mm(r)} mm`;
  }
  mesh.position.y += LIFT[d.tool][d.a.on || b.on ? 0 : 1];
  mesh.renderOrder = 9;
  ghost.add(mesh);
  tip(sized() ? text : 'drag further, or let go for a default one', event.clientX, event.clientY);
}

export function drawUp(event) {
  const was = d;
  d = null;
  clearGroup(ghost);
  tip('');
  if (!was) return;
  d = was;
  const big = sized();
  d = null;
  commit(was.tool, was.a, big ? was.b : null);
  if (!event.shiftKey) hooks.setTool?.('select');
}

export function drawCancel() {
  d = null;
  j = null;
  clearGroup(ghost);
  tip('');
}

export const drawing = () => d !== null;

// ---------------------------------------------------------------- joints

let j = null;   // a joint being dragged out from a point

const snapped = (port) => ({ part: port.part, at: snapPort(partByName(port.part).shape, port.at).at });

/** Tie port `a` to port `b`, or to the ground when there is no `b`. */
export function makeJoint(a, b) {
  if (b && a.part === b.part && JSON.stringify(a.at) === JSON.stringify(b.at)) return toast('a joint needs two different points', true);
  const joint = { type: 'joint', name: freshName('joint', listOf('joint')), kind: 'spring', a, stiffness: 1e4 };
  if (b) joint.b = b;
  S.sel = [{ kind: 'joint', name: joint.name }];
  ops([joint]);
}

/** A press in the Joint tool: on a part it may become a drag to the second point. */
export function jointDown(event, hit) {
  const port = portFromHit(hit);
  if (!port) return false;
  j = { from: snapped(port), point: hit.point.clone(), x: event.clientX, y: event.clientY, moved: false };
  return true;
}

export function jointMove(event) {
  if (!j) return;
  if (!j.moved && Math.hypot(event.clientX - j.x, event.clientY - j.y) < 5) return;
  j.moved = true;
  const hit = hitAt(event, 'part');
  const to = hit?.point ?? groundAt(event) ?? j.point;
  clearGroup(ghost);
  const line = new THREE.Line(new THREE.BufferGeometry().setFromPoints([j.point, to]), new THREE.LineBasicMaterial({ color: 0x6fbf73, depthTest: false }));
  line.renderOrder = 9;
  ghost.add(line);
  tip(hit ? `tie to ${hit.pick.name}` : 'let go here to tie it to the ground', event.clientX, event.clientY);
}

/** Returns true when the press was a drag and made a joint. */
export function jointUp(event) {
  const was = j;
  j = null;
  clearGroup(ghost);
  tip('');
  if (!was?.moved) return false;
  const port = portFromHit(hitAt(event, 'part'));
  makeJoint(was.from, port ? snapped(port) : null);
  flash(was.point, 0x6fbf73);
  return true;
}

export const jointing = () => j !== null;
