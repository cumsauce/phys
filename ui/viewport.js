// The 3D view: the scene drawn from the backend's description, picking,
// selection highlight, the move gizmo, labels and the vibration display.

import * as THREE from 'three';
import { OrbitControls } from './vendor/OrbitControls.js';
import { TransformControls } from './vendor/TransformControls.js';
import { S, $, hooks, COLORS, deg, rad, round, findItem, partByName, isSelected, listOf } from './state.js';
import { ops } from './net.js';

export { THREE };
export const SELECTED = 0xe8a33d;

const view = $('view');
export const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
view.prepend(renderer.domElement);
export const canvas = renderer.domElement;

export const scene3 = new THREE.Scene();
scene3.background = new THREE.Color(0x16181c);
export const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 100);
camera.position.set(0.55, 0.5, 0.75);

export const orbit = new OrbitControls(camera, canvas);
orbit.target.set(0, 0.05, 0);
orbit.enableDamping = true;
orbit.dampingFactor = 0.12;

scene3.add(new THREE.HemisphereLight(0xffffff, 0x30343c, 1.6));
const sun = new THREE.DirectionalLight(0xffffff, 1.4);
sun.position.set(1, 2, 1.5);
scene3.add(sun);
scene3.add(new THREE.GridHelper(2, 20, 0x3a404a, 0x262a31));

export const world = new THREE.Group();     // everything rebuilt from the scene
scene3.add(world);
export const overlay = new THREE.Group();   // handles, flashes, drag previews
scene3.add(overlay);

export const gizmo = new TransformControls(camera, canvas);
gizmo.setSize(0.8);
scene3.add(gizmo.getHelper());
/** True from the moment the gizmo is dragged until the next pointer press. */
export const gizmoState = { used: false };
let moveStart = null;   // positions of every selected part when a gizmo drag began
gizmo.addEventListener('dragging-changed', (e) => {
  orbit.enabled = !e.value;
  if (e.value) {
    gizmoState.used = true;
    moveStart = new Map(selectedGroups().map((g) => [g, g.position.clone()]));
  } else commitTransform();
});
gizmo.addEventListener('change', () => {
  // The other selected parts follow the one being dragged.
  const lead = gizmo.object;
  if (!gizmo.dragging || !lead || !moveStart?.has(lead)) return;
  const delta = lead.position.clone().sub(moveStart.get(lead));
  for (const [g, start] of moveStart) if (g !== lead) g.position.copy(start).add(delta);
});

export function setSnapping() {
  gizmo.setTranslationSnap(S.alt ? null : 0.01);
  gizmo.setRotationSnap(S.alt ? null : rad(15));
}
setSnapping();

const raycaster = new THREE.Raycaster();
raycaster.params.Line.threshold = 0.004;

function resize() {
  const w = view.clientWidth, h = view.clientHeight;
  renderer.setSize(w, h);
  camera.aspect = w / Math.max(h, 1);
  camera.updateProjectionMatrix();
}
new ResizeObserver(resize).observe(view);
resize();

// ---------------------------------------------------------------- point maths

/** A port coordinate as a point in the part's own frame (mirrors the backend). */
export function localPoint(shape, at) {
  const g = (i) => (at?.[i] ?? 0.5);
  switch (shape.kind) {
    case 'string':
    case 'bar': return new THREE.Vector3((g(0) - 0.5) * shape.length, 0, 0);
    case 'plate': return new THREE.Vector3((g(0) - 0.5) * shape.size[0], 0, (g(1) - 0.5) * shape.size[1]);
    case 'membrane': return new THREE.Vector3((2 * g(0) - 1) * shape.radius, 0, (2 * g(1) - 1) * shape.radius);
    default: return new THREE.Vector3();
  }
}

/** A point in the part's own frame as a port coordinate. */
export function portAt(shape, p) {
  const c = (x) => Math.min(0.999, Math.max(0.001, x));
  switch (shape.kind) {
    case 'string':
    case 'bar': return [round(c(p.x / shape.length + 0.5))];
    case 'plate': return [round(c(p.x / shape.size[0] + 0.5)), round(c(p.z / shape.size[1] + 0.5))];
    case 'membrane': {
      let x = p.x / shape.radius, z = p.z / shape.radius;
      const r = Math.hypot(x, z);
      if (r > 0.98) { x *= 0.98 / r; z *= 0.98 / r; }
      return [round((x + 1) / 2), round((z + 1) / 2)];
    }
    default: return [];
  }
}

export const partGroup = (name) => world.getObjectByName('part:' + name);

export function portWorld(port) {
  const group = partGroup(port.part);
  const part = partByName(port.part);
  if (!group || !part) return new THREE.Vector3();
  return group.localToWorld(localPoint(part.shape, port.at || []));
}

/** The port on `partName` nearest a point in the world. */
export function worldToPort(partName, point) {
  const group = partGroup(partName);
  const part = partByName(partName);
  if (!group || !part) return null;
  return { part: partName, at: portAt(part.shape, group.worldToLocal(point.clone())) };
}

export function partNormal(name) {
  const group = partGroup(name);
  const n = new THREE.Vector3(0, 1, 0);
  return group ? n.applyQuaternion(group.quaternion) : n;
}

// ---------------------------------------------------------------- building the world

function dispose(obj) {
  obj.traverse((o) => {
    o.geometry?.dispose();
    o.material?.dispose();
  });
}

export function clearGroup(group) {
  for (const child of [...group.children]) { group.remove(child); dispose(child); }
}

function marker(geometry, color, pick, position) {
  const m = new THREE.Mesh(geometry, new THREE.MeshStandardMaterial({ color, roughness: 0.5 }));
  m.position.copy(position);
  m.userData = { pick, color };
  world.add(m);
  return m;
}

/** A straight line, or a zig-zag for a spring. */
function coil(a, b, spring) {
  if (!spring) return [a, b];
  const pts = [a.clone()];
  const axis = b.clone().sub(a);
  const side = new THREE.Vector3(1, 0, 0);
  if (Math.abs(axis.clone().normalize().x) > 0.9) side.set(0, 0, 1);
  side.cross(axis).normalize().multiplyScalar(0.006);
  const turns = 8;
  for (let i = 1; i < turns; i++) {
    pts.push(a.clone().addScaledVector(axis, i / turns).addScaledVector(side, i % 2 ? 1 : -1));
  }
  pts.push(b.clone());
  return pts;
}

const labels = [];   // key caps: {el, object, exciter}

export function buildWorld() {
  gizmo.detach();
  clearGroup(world);
  for (const l of labels) l.el.remove();
  labels.length = 0;
  const doc = S.doc;
  let offset = 0;
  doc.scene.parts.forEach((part, i) => {
    const info = doc.parts[i];
    const group = new THREE.Group();
    group.name = 'part:' + part.name;
    group.position.fromArray(part.pos || [0, 0, 0]);
    group.rotation.order = 'ZYX';   // the backend turns about X, then Y, then Z
    group.rotation.set(rad(part.rot?.[0] || 0), rad(part.rot?.[1] || 0), rad(part.rot?.[2] || 0));
    let color = COLORS[typeof part.material === 'string' ? part.material : ''] ?? 0xb0b6c0;
    let mesh;
    if (info.kind === 'surface') {
      const geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(info.verts), 3));
      geo.setIndex(info.tris);
      geo.computeVertexNormals();
      mesh = new THREE.Mesh(geo, new THREE.MeshStandardMaterial({ color, side: THREE.DoubleSide, roughness: 0.6, metalness: 0.1, flatShading: true }));
    } else {
      const r = Math.max(0.008, Math.cbrt(3 * part.shape.mass / (4 * Math.PI * 7850)));
      color = 0x8892a0;
      mesh = new THREE.Mesh(new THREE.SphereGeometry(r, 20, 14), new THREE.MeshStandardMaterial({ color, roughness: 0.4, metalness: 0.5 }));
    }
    mesh.userData = {
      pick: { kind: 'part', name: part.name },
      base: new Float32Array(info.verts),
      shapes: info.shapes.map((s) => new Float32Array(s)),
      offset,
      kind: info.kind,
      color,
    };
    offset += info.shapes.length;
    group.add(mesh);
    if (part.shape.kind === 'string') {
      // A string is too thin to click: an unseen sleeve around it catches
      // presses that land close by.
      const sleeve = new THREE.Mesh(
        new THREE.CylinderGeometry(0.004, 0.004, part.shape.length, 6).rotateZ(Math.PI / 2),
        new THREE.MeshBasicMaterial({ visible: false }),
      );
      sleeve.userData = { pick: { kind: 'part', name: part.name }, color };
      group.add(sleeve);
    }
    world.add(group);
  });
  world.updateMatrixWorld(true);

  const dot = new THREE.SphereGeometry(0.0045, 12, 8);
  for (const j of doc.scene.joints) {
    const a = portWorld(j.a);
    const dir = j.dir ? new THREE.Vector3().fromArray(j.dir).normalize() : partNormal(j.a.part);
    const b = j.b ? portWorld(j.b) : a.clone().addScaledVector(dir, -0.06);
    const color = j.kind === 'contact' ? 0xe5604d : 0x6fbf73;
    const line = new THREE.Line(
      new THREE.BufferGeometry().setFromPoints(coil(a, b, j.kind === 'spring')),
      new THREE.LineBasicMaterial({ color }),
    );
    line.userData = { pick: { kind: 'joint', name: j.name }, color };
    world.add(line);
    marker(dot, color, { kind: 'joint', name: j.name, end: 'a' }, a);
    const foot = j.b ? dot : new THREE.BoxGeometry(0.03, 0.003, 0.03);
    marker(foot, color, { kind: 'joint', name: j.name, end: 'b' }, b);
  }

  for (const e of doc.scene.exciters) {
    const p = portWorld(e.on);
    const n = e.dir ? new THREE.Vector3().fromArray(e.dir).normalize() : partNormal(e.on.part);
    const cone = marker(new THREE.ConeGeometry(0.007, 0.024, 12), 0xd06ad8, { kind: 'exciter', name: e.name, end: 'on' }, p.clone().addScaledVector(n, 0.015));
    cone.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), n.clone().negate());   // tip toward the surface
    cone.userData.anchor = n.clone().multiplyScalar(0.015);
    if (e.key) {
      const el = document.createElement('div');
      el.className = 'cap';
      el.textContent = e.key.toUpperCase();
      $('labels').append(el);
      labels.push({ el, object: cone, exciter: e.name });
    }
  }

  for (const l of doc.scene.listeners) {
    if (l.kind === 'mic') {
      const m = marker(new THREE.SphereGeometry(0.014, 16, 12), 0x5aa2e8, { kind: 'listener', name: l.name }, new THREE.Vector3().fromArray(l.pos));
      m.name = 'mic:' + l.name;
    } else {
      marker(new THREE.BoxGeometry(0.012, 0.006, 0.012), 0x5aa2e8, { kind: 'listener', name: l.name, end: 'on' }, portWorld(l.on));
    }
  }
  applySelection();
}

/** Light the key cap of a play point for a moment. */
export function lightCap(exciter, on = true) {
  for (const l of labels) {
    if (l.exciter !== exciter) continue;
    l.el.classList.toggle('lit', on);
    if (on) setTimeout(() => l.el.classList.remove('lit'), 160);
  }
}

// ---------------------------------------------------------------- selection and the gizmo

function selectedGroups() {
  return S.sel.filter((s) => s.kind === 'part').map((s) => partGroup(s.name)).filter(Boolean);
}

let hovered = null;

export function applySelection() {
  world.traverse((o) => {
    const pick = o.userData.pick;
    if (!pick || !o.material) return;
    const on = isSelected(pick.kind, pick.name);
    const hot = hovered && hovered.kind === pick.kind && hovered.name === pick.name;
    if (o.material.emissive) o.material.emissive.setHex(on ? 0x5a3c08 : hot ? 0x26282c : 0x000000);
    else o.material.color.setHex(on ? SELECTED : o.userData.color);
  });
  gizmo.detach();
  if (S.tool === 'select' && !S.placing) {
    const groups = selectedGroups();
    const mic = S.sel.length === 1 && S.sel[0].kind === 'listener' ? world.getObjectByName('mic:' + S.sel[0].name) : null;
    if (groups.length) gizmo.attach(groups[0]);
    else if (mic) { gizmo.setMode('translate'); gizmo.attach(mic); }
  }
  hooks.handles?.();
  hooks.buttons?.();
}

function commitTransform() {
  const obj = gizmo.object;
  if (!obj) return;
  const pos = (o) => o.position.toArray().map((x) => round(x, 1e4));
  if (obj.name.startsWith('mic:')) {
    const item = findItem(S.sel[0]);
    if (item) ops([{ type: 'listener', ...item, pos: pos(obj) }]);
    return;
  }
  const batch = [];
  for (const g of selectedGroups()) {
    const part = partByName(g.name.slice(5));
    if (!part) continue;
    const rot = [g.rotation.x, g.rotation.y, g.rotation.z].map((r) => round(deg(r), 100));
    batch.push({ type: 'part', ...part, pos: pos(g), rot });
  }
  ops(batch);
}

/** Highlight what is under the cursor and say what it is. */
export function hover(event) {
  const hit = hitAt(event);
  const pick = hit?.object.userData.pick ?? null;
  const same = (pick?.kind === hovered?.kind) && (pick?.name === hovered?.name);
  if (!same) {
    hovered = pick;
    applySelection();
  }
  canvas.style.cursor = pick && S.tool === 'select' ? 'pointer' : '';
  if (pick && !same) tip(pick.name, event.clientX, event.clientY, 900);
}

// ---------------------------------------------------------------- picking

export function mouseRay(event) {
  const rect = canvas.getBoundingClientRect();
  const ndc = new THREE.Vector2(
    ((event.clientX - rect.left) / rect.width) * 2 - 1,
    -((event.clientY - rect.top) / rect.height) * 2 + 1,
  );
  raycaster.setFromCamera(ndc, camera);
  return raycaster.ray;
}

/** The nearest pickable thing under the cursor; `only` narrows it to one kind. */
export function hitAt(event, only = null) {
  mouseRay(event);
  const hits = raycaster.intersectObjects(world.children, true)
    .filter((h) => h.object.userData.pick && (!only || h.object.userData.pick.kind === only));
  if (!hits.length) return null;
  // Small things win over what they sit on: an attachment's marker first
  // (it can be dragged), then any joint line, then the surface.
  const first = hits[0];
  const close = hits.filter((h) => h.distance < first.distance + 0.02);
  const hit = close.find((h) => h.object.userData.pick.end)
    ?? close.find((h) => h.object.userData.pick.kind !== 'part')
    ?? first;
  hit.pick = hit.object.userData.pick;
  return hit;
}

/** Hits against arbitrary objects (the resize handles). */
export function hitObjects(event, objects) {
  mouseRay(event);
  return raycaster.intersectObjects(objects, false)[0] ?? null;
}

/** A hit on a part as a port {part, at}, or null. */
export function portFromHit(hit) {
  if (hit?.pick?.kind !== 'part') return null;
  const part = partByName(hit.pick.name);
  const local = hit.object.parent.worldToLocal(hit.point.clone());
  return { part: part.name, at: portAt(part.shape, local) };
}

/** Where the cursor's ray meets the ground. */
export function groundAt(event) {
  const p = new THREE.Vector3();
  return mouseRay(event).intersectPlane(new THREE.Plane(new THREE.Vector3(0, 1, 0), 0), p) ? p : null;
}

// ---------------------------------------------------------------- small feedback

export function flash(point, color = SELECTED) {
  const dot = new THREE.Mesh(new THREE.SphereGeometry(0.006, 12, 8), new THREE.MeshBasicMaterial({ color, transparent: true }));
  dot.position.copy(point);
  dot.userData.born = performance.now();
  overlay.add(dot);
}

let tipTimer = null;
/** A small label by the cursor; it goes away after `ms` (0: stays until hidden). */
export function tip(text, x, y, ms = 0) {
  const t = $('tip');
  clearTimeout(tipTimer);
  if (!text) { t.style.display = 'none'; return; }
  const rect = view.getBoundingClientRect();
  t.textContent = text;
  t.style.display = 'block';
  t.style.left = `${x - rect.left + 14}px`;
  t.style.top = `${y - rect.top + 12}px`;
  if (ms) tipTimer = setTimeout(() => { t.style.display = 'none'; }, ms);
}

// ---------------------------------------------------------------- camera

/** Frame the selection, or everything. */
export function frame() {
  const box = new THREE.Box3();
  const groups = selectedGroups();
  for (const g of groups.length ? groups : world.children) box.expandByObject(g);
  if (box.isEmpty()) return;
  const centre = box.getCenter(new THREE.Vector3());
  const size = Math.max(box.getSize(new THREE.Vector3()).length(), 0.1);
  const away = camera.position.clone().sub(orbit.target).normalize();
  orbit.target.copy(centre);
  camera.position.copy(centre).addScaledVector(away, size * 1.3);
}

/** Look along an axis: 'front', 'side' or 'top'. */
export function lookFrom(where) {
  const d = camera.position.distanceTo(orbit.target);
  const dir = { front: [0, 0.0001, 1], side: [1, 0.0001, 0], top: [0, 1, 0.0001] }[where];
  camera.position.copy(orbit.target).addScaledVector(new THREE.Vector3(...dir).normalize(), d);
}

// ---------------------------------------------------------------- every frame

let scalePeak = 1e-9;
const phases = [];

function vibrate(dt) {
  const show = $('vibes').checked;
  const amps = S.amps;
  let top = 0;
  for (const a of amps) top = Math.max(top, Math.abs(a));
  // Normalise to the loudest recent motion, which fades slowly, so a decay
  // looks like a decay.
  scalePeak = Math.max(scalePeak * Math.pow(0.5, dt / 6), top, 1e-10);
  for (const group of world.children) {
    const mesh = group.isGroup ? group.children[0] : null;
    const u = mesh?.userData;
    if (!u?.shapes) continue;
    if (u.kind === 'point') {
      // A mass: its three amplitudes are its displacement.
      const s = show ? 0.03 / Math.max(scalePeak, 1e-7) : 0;
      mesh.position.set((amps[u.offset] || 0) * s, (amps[u.offset + 1] || 0) * s, (amps[u.offset + 2] || 0) * s);
      mesh.position.clampLength(0, 0.05);
      continue;
    }
    const pos = mesh.geometry.attributes.position;
    let moving = false;
    if (show) {
      for (let k = 0; k < u.shapes.length; k++) {
        const a = Math.abs(amps[u.offset + k] || 0) / scalePeak;
        if (a < 1e-3) continue;
        if (!moving) pos.array.set(u.base);
        moving = true;
        const id = u.offset + k;
        // Slowed far down: audio-rate motion would be a blur.
        phases[id] = (phases[id] || 0) + dt * 2 * Math.PI * (1.2 + 0.55 * k);
        const w = 0.02 * Math.pow(a, 0.6) * Math.cos(phases[id]);
        const sh = u.shapes[k];
        for (let i = 0; i < sh.length; i++) pos.array[i] += w * sh[i];
      }
    }
    if (!moving && u.wasMoving) pos.array.set(u.base);
    if (moving || u.wasMoving) pos.needsUpdate = true;
    u.wasMoving = moving;
  }
}

const projected = new THREE.Vector3();
function placeLabels() {
  const w = canvas.clientWidth, h = canvas.clientHeight;
  for (const l of labels) {
    projected.copy(l.object.position).project(camera);
    const behind = projected.z > 1;
    l.el.style.display = behind ? 'none' : '';
    l.el.style.left = `${(projected.x + 1) / 2 * w}px`;
    l.el.style.top = `${(1 - projected.y) / 2 * h - 16}px`;
  }
}

export function tick(dt, now) {
  orbit.update();
  if (S.doc) vibrate(dt);
  for (const m of [...overlay.children]) {
    if (!m.userData.born) continue;
    const age = (now - m.userData.born) / 600;
    if (age >= 1 && !m.userData.keep) {
      overlay.remove(m);
      dispose(m);
    } else {
      m.material.opacity = Math.max(0.25, 1 - age);
      m.scale.setScalar(1 + Math.min(age, 1) * 1.5);
    }
  }
  placeLabels();
  renderer.render(scene3, camera);
}

/** A picture of the viewport, for an agent's `screenshot`. */
export function snapshot() {
  renderer.render(scene3, camera);
  return canvas.toDataURL('image/jpeg', 0.85);
}

// Kept for the inspector's "what refers to this" lists.
export const attachments = (partName) => ({
  joints: listOf('joint').filter((j) => j.a.part === partName || j.b?.part === partName),
  exciters: listOf('exciter').filter((e) => e.on.part === partName),
  listeners: listOf('listener').filter((l) => l.on?.part === partName),
});
