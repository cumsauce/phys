// Direct manipulation: resize handles on the selected part, dragging the
// selected parts over the ground, and dragging attachments (joint ends,
// pickups, play points) along and between parts.

import { S, hooks, round, clone, findItem, partByName } from './state.js';
import { ops, toast } from './net.js';
import {
  THREE, overlay, hitObjects, hitAt, portFromHit, portWorld, mouseRay, partGroup, tip, clearGroup, applySelection, selectedGroups,
} from './viewport.js';

// ---------------------------------------------------------------- snapping points on a part

const FRACTIONS = [[1, 2], [1, 3], [2, 3], [1, 4], [3, 4], [1, 5], [2, 5], [3, 5], [4, 5]];

/** Snap a port coordinate to the simple fractions (the node positions of
 *  the low harmonics). Returns the coordinate and a label for it. */
export function snapPort(shape, at) {
  if (S.alt || shape.kind === 'mass') return { at, label: '' };
  const names = [];
  const snapped = at.map((x) => {
    for (const [n, d] of FRACTIONS) {
      if (Math.abs(x - n / d) < 0.018) { names.push(`${n}/${d}`); return round(n / d); }
    }
    names.push(x.toFixed(2));
    return x;
  });
  if (shape.kind === 'membrane') {
    const centre = Math.hypot(at[0] - 0.5, at[1] - 0.5) < 0.03;
    return centre ? { at: [0.5, 0.5], label: 'centre' } : { at, label: '' };
  }
  return { at: snapped, label: names.join(', ') };
}

// ---------------------------------------------------------------- resize handles

const handles = new THREE.Group();
overlay.add(handles);
const HANDLE = new THREE.BoxGeometry(0.012, 0.012, 0.012);

/** What can be dragged on each shape: a local axis, a side, and how the
 *  size reads and writes. */
function handleSpecs(shape) {
  const len = (get, set, label) => ({ get, set, label });
  switch (shape.kind) {
    case 'string':
      return [-1, 1].map((sign) => ({ axis: 'x', sign, ...len((s) => s.length, (s, v) => { s.length = v; }, 'length') }));
    case 'bar':
      return [
        ...[-1, 1].map((sign) => ({ axis: 'x', sign, ...len((s) => s.length, (s, v) => { s.length = v; }, 'length') })),
        ...[-1, 1].map((sign) => ({ axis: 'z', sign, ...len((s) => s.width, (s, v) => { s.width = v; }, 'width') })),
      ];
    case 'plate':
      return [
        ...[-1, 1].map((sign) => ({ axis: 'x', sign, ...len((s) => s.size[0], (s, v) => { s.size[0] = v; }, 'size x') })),
        ...[-1, 1].map((sign) => ({ axis: 'z', sign, ...len((s) => s.size[1], (s, v) => { s.size[1] = v; }, 'size z') })),
      ];
    case 'membrane':
      return [['x', -1], ['x', 1], ['z', -1], ['z', 1]].map(([axis, sign]) => ({
        axis, sign, radial: true, ...len((s) => s.radius, (s, v) => { s.radius = v; }, 'radius'),
      }));
    default: return [];
  }
}

function showHandles() {
  clearGroup(handles);
  if (S.tool !== 'select' || S.sel.length !== 1 || S.sel[0].kind !== 'part') return;
  const part = partByName(S.sel[0].name);
  const group = partGroup(S.sel[0].name);
  if (!part || !group) return;
  for (const spec of handleSpecs(part.shape)) {
    const size = spec.get(part.shape);
    const reach = spec.radial ? size : size / 2;
    const local = new THREE.Vector3(spec.axis === 'x' ? spec.sign * reach : 0, 0, spec.axis === 'z' ? spec.sign * reach : 0);
    const h = new THREE.Mesh(HANDLE, new THREE.MeshBasicMaterial({ color: 0xffffff, depthTest: false }));
    h.renderOrder = 10;
    h.position.copy(group.localToWorld(local));
    h.quaternion.copy(group.quaternion);
    h.userData.handle = { spec, part: part.name };
    handles.add(h);
  }
}
hooks.handles = showHandles;

let drag = null;   // the resize or attachment drag under way

/** Start resizing if the press landed on a handle. */
export function handleDown(event) {
  const hit = hitObjects(event, handles.children);
  if (!hit) return false;
  const { spec, part: name } = hit.object.userData.handle;
  const part = partByName(name);
  const group = partGroup(name);
  const axis = new THREE.Vector3(spec.axis === 'x' ? 1 : 0, 0, spec.axis === 'z' ? 1 : 0)
    .applyQuaternion(group.quaternion).multiplyScalar(spec.sign);
  drag = {
    kind: 'resize', spec, part, group, axis,
    origin: hit.object.position.clone(),
    startPos: group.position.clone(),
    size: spec.get(part.shape),
    next: spec.get(part.shape),
    handle: hit.object,
  };
  return true;
}

/** How far along a line (origin, unit direction) the cursor's ray comes closest. */
function alongAxis(event, origin, dir) {
  const ray = mouseRay(event);
  const w = origin.clone().sub(ray.origin);
  const b = dir.dot(ray.direction);
  const d = dir.dot(w), e = ray.direction.dot(w);
  const denom = 1 - b * b;
  return Math.abs(denom) < 1e-6 ? 0 : (b * e - d) / denom;
}

function resizeMove(event) {
  const d = drag;
  const t = alongAxis(event, d.origin, d.axis);
  const step = S.alt ? 0.0005 : 0.01;
  const least = d.spec.radial ? 0.02 : 0.01;
  d.next = Math.max(least, Math.round((d.size + t) / step) * step);
  const ratio = d.next / d.size;
  // Preview by stretching the drawn part; the far side stays where it is.
  d.group.scale.set(1, 1, 1);
  if (d.spec.radial) d.group.scale.set(ratio, 1, ratio);
  else {
    d.group.scale[d.spec.axis] = ratio;
    d.group.position.copy(d.startPos).addScaledVector(d.axis, (d.next - d.size) / 2);
  }
  d.handle.position.copy(d.origin).addScaledVector(d.axis, d.next - d.size);
  tip(`${d.spec.label} ${(d.next * 1000).toFixed(S.alt ? 1 : 0)} mm`, event.clientX, event.clientY);
}

function resizeUp() {
  const d = drag;
  tip('');
  if (Math.abs(d.next - d.size) < 1e-9) {
    d.group.scale.set(1, 1, 1);
    d.group.position.copy(d.startPos);
    showHandles();
    return;
  }
  const part = clone(d.part);
  d.spec.set(part.shape, round(d.next, 1e5));
  if (!d.spec.radial) part.pos = d.group.position.toArray().map((x) => round(x, 1e5));
  ops([{ type: 'part', ...part }]);
}

// ---------------------------------------------------------------- dragging parts over the ground

/** Start sliding the selected parts level with the ground, from a press
 *  that landed on one of them. */
export function moveDragStart(hit) {
  const groups = selectedGroups();
  if (!groups.length) return false;
  clearGroup(handles);
  drag = {
    kind: 'move',
    plane: new THREE.Plane(new THREE.Vector3(0, 1, 0), -hit.point.y),
    from: hit.point.clone(),
    groups: groups.map((g) => [g, g.position.clone()]),
    delta: new THREE.Vector3(),
  };
  return true;
}

function moveMove(event) {
  const d = drag;
  const p = new THREE.Vector3();
  if (!mouseRay(event).intersectPlane(d.plane, p)) return;
  d.delta.copy(p).sub(d.from).setY(0).clampLength(0, 5);
  if (!S.alt) { d.delta.x = Math.round(d.delta.x * 100) / 100; d.delta.z = Math.round(d.delta.z * 100) / 100; }
  for (const [g, start] of d.groups) g.position.copy(start).add(d.delta);
  tip(`${(d.delta.x * 1000).toFixed(0)}, ${(d.delta.z * 1000).toFixed(0)} mm`, event.clientX, event.clientY);
}

function moveUp() {
  const d = drag;
  tip('');
  if (d.delta.lengthSq() < 1e-12) return showHandles();
  const batch = [];
  for (const [g] of d.groups) {
    const part = partByName(g.name.slice(5));
    if (part) batch.push({ type: 'part', ...clone(part), pos: g.position.toArray().map((x) => round(x, 1e4)) });
  }
  ops(batch);
}

// ---------------------------------------------------------------- dragging attachments

/** Start dragging the attachment a marker stands for. */
export function portDragStart(hit) {
  const pick = hit.pick;
  const item = findItem(pick);
  if (!item || !pick.end) return false;
  drag = { kind: 'port', pick, item, object: hit.object, home: hit.object.position.clone(), port: null, offGround: false };
  return true;
}

function portMove(event) {
  const d = drag;
  const hit = hitAt(event, 'part');
  const raw = portFromHit(hit);
  if (!raw) {
    d.port = null;
    d.offGround = d.pick.kind === 'joint' && d.pick.end === 'b';
    d.object.position.copy(d.home);
    tip(d.offGround ? 'release to tie to the ground' : 'drop it on a part', event.clientX, event.clientY);
    return;
  }
  const snap = snapPort(partByName(raw.part).shape, raw.at);
  d.port = { part: raw.part, at: snap.at };
  d.offGround = false;
  d.object.position.copy(portWorld(d.port)).add(d.object.userData.anchor ?? new THREE.Vector3());
  tip(`${raw.part} ${snap.label}`, event.clientX, event.clientY);
}

function portUp() {
  const d = drag;
  tip('');
  const item = clone(d.item);
  if (d.pick.kind === 'joint') {
    if (d.port) item[d.pick.end] = d.port;
    else if (d.offGround) delete item.b;
    else return applySelection(), hooks.rebuild?.();
    const same = item.b && item.a.part === item.b.part && JSON.stringify(item.a.at) === JSON.stringify(item.b.at);
    if (same) { toast('a joint needs two different points', true); return hooks.rebuild?.(); }
  } else {
    if (!d.port) return hooks.rebuild?.();
    item.on = d.port;
  }
  ops([{ type: d.pick.kind, ...item }]);
}

// ---------------------------------------------------------------- the drag under way

export const dragging = () => drag !== null;

export function dragMove(event) {
  if (drag?.kind === 'resize') resizeMove(event);
  else if (drag?.kind === 'move') moveMove(event);
  else if (drag?.kind === 'port') portMove(event);
}

export function dragUp() {
  const d = drag;
  if (!d) return;
  if (d.kind === 'resize') resizeUp();
  else if (d.kind === 'move') moveUp();
  else portUp();
  drag = null;
}

export function dragCancel() {
  if (!drag) return;
  drag = null;
  tip('');
  hooks.rebuild?.();
}
