// phys editor. The backend owns the scene and the physics; this page draws
// it, sends edits as ops, and sends strikes.

import * as THREE from 'three';
import { OrbitControls } from './vendor/OrbitControls.js';
import { TransformControls } from './vendor/TransformControls.js';

const $ = (id) => document.getElementById(id);
const MATERIALS = ['steel', 'aluminium', 'brass', 'bronze', 'glass', 'wood', 'hardwood', 'stone', 'ice', 'plastic', 'nylon', 'mylar'];
const COLORS = {
  steel: 0x9aa4b0, aluminium: 0xc4cad2, brass: 0xc9a54a, bronze: 0xb07a3c, glass: 0x9fd3de,
  wood: 0xc79a62, hardwood: 0x8a5a36, stone: 0x8d8d86, ice: 0xcfe8f5, plastic: 0xd8d2c0,
  nylon: 0xe8e4d8, mylar: 0xe9e9ef, metal: 0x9aa4b0, aluminum: 0xc4cad2,
};
const SELECTED = 0xe8a33d;

// ---------------------------------------------------------------- state

let doc = null;          // the last "scene" message
let sel = null;          // {kind: 'part' | 'joint' | 'exciter' | 'listener', name}
let tool = 'select';
let jointFrom = null;    // first port of a joint being drawn
let hand = 'strike';     // how the Play tool excites: strike, pluck or bow
let bowing = null;       // the port being bowed while the mouse is down
let amps = [];           // latest mode amplitudes, in display order
let ws = null;

// ---------------------------------------------------------------- three

const view = $('view');
const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
view.appendChild(renderer.domElement);

const scene3 = new THREE.Scene();
scene3.background = new THREE.Color(0x16181c);
const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 100);
camera.position.set(0.55, 0.5, 0.75);

const orbit = new OrbitControls(camera, renderer.domElement);
orbit.target.set(0, 0.05, 0);
orbit.enableDamping = true;
orbit.dampingFactor = 0.12;

scene3.add(new THREE.HemisphereLight(0xffffff, 0x30343c, 1.6));
const sun = new THREE.DirectionalLight(0xffffff, 1.4);
sun.position.set(1, 2, 1.5);
scene3.add(sun);
const grid = new THREE.GridHelper(2, 20, 0x3a404a, 0x262a31);
scene3.add(grid);

const world = new THREE.Group();   // everything rebuilt from the scene
scene3.add(world);
const marks = new THREE.Group();   // strike flashes and the joint-in-progress dot
scene3.add(marks);

const gizmo = new TransformControls(camera, renderer.domElement);
gizmo.setSize(0.8);
scene3.add(gizmo.getHelper());
let gizmoUsed = false;
gizmo.addEventListener('dragging-changed', (e) => {
  orbit.enabled = !e.value;
  if (e.value) gizmoUsed = true;
  else commitTransform();
});

const raycaster = new THREE.Raycaster();
raycaster.params.Line.threshold = 0.008;

function resize() {
  const w = view.clientWidth, h = view.clientHeight;
  renderer.setSize(w, h);
  camera.aspect = w / Math.max(h, 1);
  camera.updateProjectionMatrix();
}
new ResizeObserver(resize).observe(view);

// ---------------------------------------------------------------- geometry helpers

const deg = (r) => r * 180 / Math.PI;
const rad = (d) => d * Math.PI / 180;
const round = (x, p = 1e5) => Math.round(x * p) / p;

function partByName(name) {
  return doc.scene.parts.find((p) => p.name === name);
}

/** A port coordinate as a point in the part's own frame (mirrors the backend). */
function localPoint(shape, at) {
  const g = (i) => (at[i] ?? 0.5);
  switch (shape.kind) {
    case 'string':
    case 'bar': return new THREE.Vector3((g(0) - 0.5) * shape.length, 0, 0);
    case 'plate': return new THREE.Vector3((g(0) - 0.5) * shape.size[0], 0, (g(1) - 0.5) * shape.size[1]);
    case 'membrane': return new THREE.Vector3((2 * g(0) - 1) * shape.radius, 0, (2 * g(1) - 1) * shape.radius);
    default: return new THREE.Vector3();
  }
}

/** A point in the part's own frame as a port coordinate. */
function portAt(shape, p) {
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

function portWorld(port) {
  const group = world.getObjectByName('part:' + port.part);
  const part = partByName(port.part);
  if (!group || !part) return new THREE.Vector3();
  return group.localToWorld(localPoint(part.shape, port.at || []));
}

function partNormal(name) {
  const group = world.getObjectByName('part:' + name);
  const n = new THREE.Vector3(0, 1, 0);
  return group ? n.applyQuaternion(group.quaternion) : n;
}

// ---------------------------------------------------------------- building the 3D scene

function dispose(obj) {
  obj.traverse((o) => {
    o.geometry?.dispose();
    o.material?.dispose();
  });
}

function buildWorld() {
  gizmo.detach();
  for (const child of [...world.children]) { world.remove(child); dispose(child); }
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
      mesh = new THREE.Mesh(geo, new THREE.MeshStandardMaterial({ color, side: THREE.DoubleSide, roughness: 0.6, metalness: 0.1 }));
    } else if (info.kind === 'line') {
      const geo = new THREE.BufferGeometry();
      geo.setAttribute('position', new THREE.BufferAttribute(new Float32Array(info.verts), 3));
      color = new THREE.Color(color).lerp(new THREE.Color(0xffffff), 0.5).getHex();   // a thin line needs the contrast
      mesh = new THREE.Line(geo, new THREE.LineBasicMaterial({ color }));
    } else {
      const r = Math.max(0.008, Math.cbrt(3 * part.shape.mass / (4 * Math.PI * 7850)));
      mesh = new THREE.Mesh(new THREE.SphereGeometry(r, 20, 14), new THREE.MeshStandardMaterial({ color: 0x8892a0, roughness: 0.4, metalness: 0.5 }));
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
    world.add(group);
  });
  world.updateMatrixWorld(true);

  for (const j of doc.scene.joints) {
    const a = portWorld(j.a);
    const dir = j.dir ? new THREE.Vector3().fromArray(j.dir).normalize() : partNormal(j.a.part);
    const b = j.b ? portWorld(j.b) : a.clone().addScaledVector(dir, -0.06);
    const color = j.kind === 'contact' ? 0xe5604d : 0x6fbf73;
    const line = new THREE.Line(
      new THREE.BufferGeometry().setFromPoints(coil(a, b, j.kind === 'spring')),
      new THREE.LineBasicMaterial({ color }),
    );
    line.name = 'joint:' + j.name;
    line.userData = { pick: { kind: 'joint', name: j.name }, color };
    world.add(line);
    if (!j.b) {
      const foot = new THREE.Mesh(new THREE.BoxGeometry(0.03, 0.003, 0.03), new THREE.MeshBasicMaterial({ color }));
      foot.position.copy(b);
      foot.userData = { pick: { kind: 'joint', name: j.name }, color };
      world.add(foot);
    }
  }

  for (const e of doc.scene.exciters) {
    const p = portWorld(e.on);
    const n = e.dir ? new THREE.Vector3().fromArray(e.dir).normalize() : partNormal(e.on.part);
    const cone = new THREE.Mesh(new THREE.ConeGeometry(0.008, 0.03, 12), new THREE.MeshStandardMaterial({ color: 0xd06ad8 }));
    cone.quaternion.setFromUnitVectors(new THREE.Vector3(0, 1, 0), n.clone().negate());   // tip toward the surface
    cone.position.copy(p).addScaledVector(n, 0.018);
    cone.name = 'exciter:' + e.name;
    cone.userData = { pick: { kind: 'exciter', name: e.name }, color: 0xd06ad8 };
    world.add(cone);
  }

  for (const l of doc.scene.listeners) {
    let obj;
    if (l.kind === 'mic') {
      obj = new THREE.Mesh(new THREE.SphereGeometry(0.014, 16, 12), new THREE.MeshStandardMaterial({ color: 0x5aa2e8 }));
      obj.position.fromArray(l.pos);
    } else {
      obj = new THREE.Mesh(new THREE.BoxGeometry(0.014, 0.006, 0.014), new THREE.MeshStandardMaterial({ color: 0x5aa2e8 }));
      obj.position.copy(portWorld(l.on));
    }
    obj.name = 'listener:' + l.name;
    obj.userData = { pick: { kind: 'listener', name: l.name }, color: 0x5aa2e8 };
    world.add(obj);
  }
  applySelection();
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

function selectedObject() {
  if (!sel) return null;
  return world.getObjectByName(sel.kind + ':' + sel.name) ?? null;
}

function applySelection() {
  world.traverse((o) => {
    const pick = o.userData.pick;
    if (!pick || !o.material) return;
    const on = sel && pick.kind === sel.kind && pick.name === sel.name;
    if (o.material.emissive) o.material.emissive.setHex(on ? 0x5a3c08 : 0x000000);
    else o.material.color.setHex(on ? SELECTED : o.userData.color);
  });
  gizmo.detach();
  const obj = selectedObject();
  if (tool === 'select' && obj) {
    if (sel.kind === 'part') gizmo.attach(obj);
    else if (sel.kind === 'listener' && findItem(sel)?.kind === 'mic') { gizmo.setMode('translate'); gizmo.attach(obj); }
  }
  syncButtons();
}

function commitTransform() {
  const obj = gizmo.object;
  if (!obj || !sel) return;
  const item = findItem(sel);
  if (!item) return;
  const pos = obj.position.toArray().map((x) => round(x, 1e4));
  if (sel.kind === 'part') {
    const rot = [obj.rotation.x, obj.rotation.y, obj.rotation.z].map((r) => round(deg(r), 100));
    send({ type: 'ops', ops: [{ type: 'part', ...item, pos, rot }] });
  } else {
    send({ type: 'ops', ops: [{ type: 'listener', ...item, pos }] });
  }
}

// ---------------------------------------------------------------- vibration display

let scalePeak = 1e-9;
const phases = [];

function animateVibration(dt) {
  const show = $('vibes').checked;
  let top = 0;
  for (const a of amps) top = Math.max(top, Math.abs(a));
  // Normalise to the loudest recent motion, which fades slowly, so a decay
  // looks like a decay.
  scalePeak = Math.max(scalePeak * Math.pow(0.5, dt / 6), top, 1e-10);
  for (const group of world.children) {
    const mesh = group.children?.[0];
    const u = mesh?.userData;
    if (!u?.shapes) continue;
    const n = u.shapes.length;
    if (u.kind === 'point') {
      // A mass: its three amplitudes are its displacement.
      const s = show ? 0.03 / Math.max(scalePeak, 1e-7) : 0;
      mesh.position.set((amps[u.offset] || 0) * s, (amps[u.offset + 1] || 0) * s, (amps[u.offset + 2] || 0) * s);
      mesh.position.clampLength(0, 0.05);
      continue;
    }
    const pos = mesh.geometry.attributes.position;
    pos.array.set(u.base);
    let moving = false;
    if (show) {
      for (let k = 0; k < n; k++) {
        const a = Math.abs(amps[u.offset + k] || 0) / scalePeak;
        if (a < 1e-3) continue;
        moving = true;
        const id = u.offset + k;
        // Slowed far down: audio-rate motion would be a blur.
        phases[id] = (phases[id] || 0) + dt * 2 * Math.PI * (1.2 + 0.55 * k);
        const w = 0.02 * Math.pow(a, 0.6) * Math.cos(phases[id]);
        const sh = u.shapes[k];
        for (let i = 0; i < sh.length; i++) pos.array[i] += w * sh[i];
      }
    }
    if (moving || u.wasMoving) {
      pos.needsUpdate = true;
      if (u.kind === 'surface') mesh.geometry.computeVertexNormals();
    }
    u.wasMoving = moving;
  }
}

// ---------------------------------------------------------------- picking and tools

function hitAt(event) {
  const rect = renderer.domElement.getBoundingClientRect();
  const ndc = new THREE.Vector2(
    ((event.clientX - rect.left) / rect.width) * 2 - 1,
    -((event.clientY - rect.top) / rect.height) * 2 + 1,
  );
  raycaster.setFromCamera(ndc, camera);
  const hits = raycaster.intersectObjects(world.children, true).filter((h) => h.object.userData.pick);
  return hits[0] ?? null;
}

/** A hit on a part as a port {part, at}, or null. */
function portFromHit(hit) {
  const pick = hit?.object.userData.pick;
  if (pick?.kind !== 'part') return null;
  const part = partByName(pick.name);
  const local = hit.object.parent.worldToLocal(hit.point.clone());
  return { part: part.name, at: portAt(part.shape, local) };
}

function flash(point, color = SELECTED) {
  const ring = new THREE.Mesh(new THREE.SphereGeometry(0.006, 12, 8), new THREE.MeshBasicMaterial({ color, transparent: true }));
  ring.position.copy(point);
  ring.userData.born = performance.now();
  marks.add(ring);
}

/** The Play panel's settings as the fields of a strike message. */
function mallet() {
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

function setHand(h) {
  hand = h;
  for (const b of $('hand').children) b.classList.toggle('on', b.dataset.hand === h);
  for (const box of document.querySelectorAll('#strikebox [data-for]')) box.style.display = box.dataset.for === h ? '' : 'none';
}

function stopBow() {
  if (!bowing) return;
  bowing = null;
  orbit.enabled = true;
  send({ type: 'bow_stop' });
}

function freshName(base, list) {
  const used = new Set(list.map((x) => x.name));
  if (!used.has(base)) return base;
  for (let i = 2; ; i++) if (!used.has(base + i)) return base + i;
}

let down = null;
renderer.domElement.addEventListener('pointerdown', (e) => {
  down = { x: e.clientX, y: e.clientY };
  gizmoUsed = false;
  if (tool === 'strike' && hand === 'bow' && e.button === 0 && doc) {
    // The bow sounds for as long as the button is held.
    const hit = hitAt(e);
    const port = portFromHit(hit);
    if (!port) return;
    bowing = port;
    orbit.enabled = false;
    send({ type: 'strike', ...port, ...mallet() });
    flash(hit.point, 0x5aa2e8);
  }
});
window.addEventListener('pointerup', stopBow);
window.addEventListener('blur', stopBow);
renderer.domElement.addEventListener('pointerup', (e) => {
  if (!down || !doc) return;
  const moved = Math.hypot(e.clientX - down.x, e.clientY - down.y) > 4;
  down = null;
  if (hand === 'bow' && tool === 'strike') return;
  if (moved || gizmoUsed || e.button !== 0) return;
  const hit = hitAt(e);
  const port = portFromHit(hit);
  if (tool === 'strike') {
    if (!port || hand === 'bow') return;
    send({ type: 'strike', ...port, ...mallet() });
    flash(hit.point);
  } else if (tool === 'joint') {
    if (!jointFrom) {
      if (!port) return toast('click a part to start the joint', true);
      jointFrom = port;
      flash(hit.point, 0x6fbf73);
      toast('now click the second point (empty space ties it to the ground)');
    } else {
      const joint = { type: 'joint', name: freshName('joint', doc.scene.joints), kind: 'spring', a: jointFrom, stiffness: 1e4 };
      if (port) joint.b = port;
      jointFrom = null;
      sel = { kind: 'joint', name: joint.name };
      send({ type: 'ops', ops: [joint] });
    }
  } else if (tool === 'pickup') {
    if (!port) return;
    const name = freshName('pickup', doc.scene.listeners);
    sel = { kind: 'listener', name };
    send({ type: 'ops', ops: [{ type: 'listener', name, kind: 'pickup', on: port }] });
  } else {
    sel = hit ? { ...hit.object.userData.pick } : null;
    applySelection();
    renderInspector();
  }
});

function setTool(t) {
  tool = t;
  jointFrom = null;
  view.className = t;
  $('strikebox').style.display = t === 'strike' ? '' : 'none';
  applySelection();
}

function syncButtons() {
  for (const b of $('tools').children) b.classList.toggle('on', b.dataset.tool === tool);
  for (const b of $('gizmo').children) b.classList.toggle('on', b.dataset.mode === gizmo.mode);
  $('undo').disabled = !doc?.undo;
  $('redo').disabled = !doc?.redo;
}

// ---------------------------------------------------------------- inspector

function findItem(s) {
  if (!s || !doc) return null;
  const list = { part: doc.scene.parts, joint: doc.scene.joints, exciter: doc.scene.exciters, listener: doc.scene.listeners }[s.kind];
  return list?.find((x) => x.name === s.name) ?? null;
}

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

/** A number field. An empty box means "leave it to the default". */
function num(label, value, set, hint = '') {
  const input = el('input', { type: 'number', step: 'any', value: value ?? '', placeholder: hint });
  input.addEventListener('change', () => set(input.value === '' ? undefined : Number(input.value)));
  return el('label', {}, label, input);
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

/** Send `item` with `patch` applied as an upsert; undefined removes a field. */
function update(kind, item, patch) {
  const next = structuredClone(item);
  for (const [k, v] of Object.entries(patch)) {
    if (v === undefined) delete next[k];
    else next[k] = v;
  }
  send({ type: 'ops', ops: [{ type: kind, ...next }] });
}

function remove() {
  if (!sel) return;
  send({ type: 'ops', ops: [{ type: 'delete', kind: sel.kind, name: sel.name }] });
  sel = null;
}

function renderInspector() {
  const box = $('inspector');
  box.replaceChildren();
  if (!doc) return;
  const item = findItem(sel);
  if (!item) { sel = null; return renderOutline(box); }
  const kind = sel.kind;
  const set = (patch) => update(kind, item, patch);
  box.append(el('h2', {}, `${kind}: ${item.name}`));

  if (kind === 'part') {
    const sh = item.shape;
    const setShape = (patch) => {
      const shape = { ...sh };
      for (const [k, v] of Object.entries(patch)) { if (v === undefined) delete shape[k]; else shape[k] = v; }
      set({ shape });
    };
    const info = doc.parts[doc.scene.parts.indexOf(item)];
    box.append(el('div', { class: 'note' }, `${sh.kind}, ${info.modes} modes, lowest ${info.hz[0]?.toFixed(1) ?? '?'} Hz`));
    if (sh.kind !== 'mass') {
      box.append(choice('Material', typeof item.material === 'string' ? item.material : 'steel', MATERIALS, (v) => set({ material: v })));
    }
    if (sh.kind === 'string') {
      box.append(num('Length m', sh.length, (v) => setShape({ length: v })));
      box.append(num('Radius m', sh.radius, (v) => setShape({ radius: v }), '0.0005'));
      if (sh.tension != null) box.append(num('Tension N', sh.tension, (v) => setShape({ tension: v })));
      else box.append(num('Pitch Hz', sh.f0, (v) => setShape({ f0: v })));
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
      box.append(num('Radius m', sh.radius, (v) => setShape({ radius: v })));
      if (sh.tension != null) box.append(num('Tension N/m', sh.tension, (v) => setShape({ tension: v })));
      else box.append(num('Pitch Hz', sh.f0, (v) => setShape({ f0: v })));
    } else {
      box.append(num('Mass kg', sh.mass, (v) => setShape({ mass: v })));
    }
    box.append(triple('Position', item.pos, (v) => set({ pos: v })));
    box.append(triple('Rotation °', item.rot, (v) => set({ rot: v })));
    if (sh.kind !== 'mass') {
      box.append(num('Loss', item.loss, (v) => set({ loss: v }), 'material'));
      box.append(num('Damping 1/s', item.damping, (v) => set({ damping: v }), '1'));
      box.append(num('Max modes', item.modes, (v) => set({ modes: v }), '96'));
    }
  } else if (kind === 'joint') {
    const end = (p) => (p ? `${p.part} [${(p.at || []).join(', ')}]` : 'ground');
    box.append(el('div', { class: 'note' }, `${end(item.a)} → ${end(item.b)}`));
    box.append(choice('Kind', item.kind, ['spring', 'contact'], (v) => set({ kind: v, cubic: undefined, exponent: undefined, gap: undefined, stiffness: v === 'contact' ? 1e8 : 1e4 })));
    box.append(num(item.kind === 'spring' ? 'Stiffness N/m' : 'Stiffness', item.stiffness, (v) => set({ stiffness: v }), item.kind === 'spring' ? '1e4' : '1e8'));
    box.append(num('Damping', item.damping, (v) => set({ damping: v }), '0'));
    box.append(num(item.kind === 'spring' ? 'Free play m' : 'Gap m', item.gap, (v) => set({ gap: v }), '0'));
    if (item.kind === 'spring') box.append(num('Cubic N/m³', item.cubic, (v) => set({ cubic: v }), '0'));
    else box.append(num('Exponent', item.exponent, (v) => set({ exponent: v }), '1.5'));
  } else if (kind === 'exciter') {
    box.append(el('div', { class: 'note' }, `${item.kind} on ${item.on.part}`));
    if (item.kind === 'mallet') {
      box.append(num('Mass kg', item.mass, (v) => set({ mass: v }), '0.02'));
      box.append(num('Stiffness', item.stiffness, (v) => set({ stiffness: v }), '1e8'));
      box.append(num('Damping', item.damping, (v) => set({ damping: v }), '0.3'));
    }
    if (item.kind === 'pluck') box.append(num('Pull time s', item.time, (v) => set({ time: v }), '0.02'));
    if (item.kind === 'bow') box.append(num('Pressure N', item.pressure, (v) => set({ pressure: v }), '0.3'));
    const fire = (strength) => () => send({ type: 'excite', exciter: item.name, strength });
    if (item.kind === 'bow') {
      box.append(el('div', { class: 'row' }, el('button', { onclick: fire(0.2) }, 'Bow'), el('button', { onclick: fire(0) }, 'Lift')));
    } else {
      const strength = { mallet: 2, impulse: 0.01, pluck: 0.003 }[item.kind];
      box.append(el('div', { class: 'row' }, el('button', { onclick: fire(strength) }, 'Fire')));
    }
  } else {
    if (item.kind === 'mic') box.append(triple('Position', item.pos, (v) => set({ pos: v })));
    else box.append(el('div', { class: 'note' }, `pickup on ${item.on.part} [${item.on.at.join(', ')}]`));
    box.append(num('Gain dB', item.gain_db, (v) => set({ gain_db: v ?? 0 }), '0'));
    box.append(num('Pan', item.pan, (v) => set({ pan: v ?? 0 }), '0'));
  }
  box.append(el('div', { class: 'row' }, el('button', { class: 'danger', onclick: remove }, 'Delete')));
}

function renderOutline(box) {
  const s = doc.scene;
  box.append(el('h2', {}, 'Scene'));
  const name = el('input', { type: 'text', value: s.name || '' });
  name.addEventListener('change', () => send({ type: 'ops', ops: [{ type: 'name', name: name.value }] }));
  box.append(el('label', {}, 'Name', name));
  const settings = s.settings || {};
  const setSettings = (patch) => {
    const next = { ...settings };
    for (const [k, v] of Object.entries(patch)) { if (v === undefined) delete next[k]; else next[k] = v; }
    send({ type: 'ops', ops: [{ type: 'settings', ...next }] });
  };
  box.append(choice('Solver', settings.solver || 'sav', ['sav', 'explicit'], (v) => setSettings({ solver: v })));
  box.append(num('Oversample', settings.oversample, (v) => setSettings({ oversample: v }), 'auto'));
  box.append(num('Top Hz', settings.max_hz, (v) => setSettings({ max_hz: v }), '16000'));
  box.append(el('div', { class: 'note' }, `${doc.info.modes} modes at ${doc.info.rate} Hz, ${doc.info.oversample}x${doc.info.audio ? '' : ' (no audio device)'}`));
  for (const w of doc.info.warnings) box.append(el('div', { class: 'warn' }, w));
  const section = (title, kind, list, describe) => {
    if (!list.length) return;
    box.append(el('h2', {}, title));
    box.append(el('ul', {}, ...list.map((x) => el('li', {
      onclick: () => { sel = { kind, name: x.name }; setTool('select'); renderInspector(); },
    }, x.name + ' ', el('small', {}, describe(x))))));
  };
  section('Parts', 'part', s.parts, (p) => p.shape.kind);
  section('Joints', 'joint', s.joints, (j) => `${j.kind} ${j.a.part}–${j.b?.part ?? 'ground'}`);
  section('Exciters', 'exciter', s.exciters, (e) => `${e.kind} on ${e.on.part}`);
  section('Listeners', 'listener', s.listeners, (l) => l.kind);
  box.append(el('div', { class: 'note' }, doc.path ? `File: ${doc.path}` : 'Not saved yet'));
}

// ---------------------------------------------------------------- adding things

function add(what) {
  const parts = doc.scene.parts;
  const y = round(0.1 + 0.08 * parts.length, 100);
  const templates = {
    plate: { shape: { kind: 'plate', size: [0.4, 0.3], thickness: 0.003 }, material: 'wood' },
    bar: { shape: { kind: 'bar', length: 0.3, width: 0.04, thickness: 0.012 }, material: 'aluminium' },
    string: { shape: { kind: 'string', length: 0.65, radius: 0.0004, f0: 196 }, material: 'steel' },
    membrane: { shape: { kind: 'membrane', radius: 0.15, f0: 120 }, material: 'mylar' },
    mass: { shape: { kind: 'mass', mass: 0.01 } },
  };
  if (what === 'mic') {
    const name = freshName('mic', doc.scene.listeners);
    sel = { kind: 'listener', name };
    return send({ type: 'ops', ops: [{ type: 'listener', name, kind: 'mic', pos: [0.2, 0.3, 0.3] }] });
  }
  const name = freshName(what, parts);
  sel = { kind: 'part', name };
  send({ type: 'ops', ops: [{ type: 'part', name, pos: [0, y, 0], ...templates[what] }] });
}

// ---------------------------------------------------------------- connection

function showFile() {
  const file = doc.path ? doc.path.split('/').pop() : 'not saved';
  $('file').textContent = `${doc.scene.name || 'untitled'}${doc.dirty ? ' *' : ''} — ${file}`;
  $('file').title = doc.path || 'This scene has not been saved to a file yet';
  document.title = `phys — ${$('file').textContent}`;
}

function toast(message, error = false) {
  const t = $('toast');
  t.textContent = message;
  t.className = error ? 'error' : '';
}

function send(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
  else toast('not connected', true);
}

function connect() {
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => {
    $('conn').textContent = 'connected';
    $('conn').className = 'good';
    send({ type: 'volume', value: Number($('volume').value) });
  };
  ws.onclose = () => {
    $('conn').textContent = 'disconnected, retrying…';
    $('conn').className = 'bad';
    setTimeout(connect, 1000);
  };
  ws.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.type === 'scene') {
      doc = msg;
      amps = [];
      toast('');
      buildWorld();
      renderInspector();
      $('stats').textContent = `${msg.info.modes} modes`;
      showFile();
      $('clear').textContent = msg.take ? `Clear take (${msg.take})` : 'Clear take';
    } else if (msg.type === 'tm') {
      amps = msg.amps;
      const db = 20 * Math.log10(Math.max(msg.peak, 1e-6));
      const level = $('level');
      level.style.width = `${Math.max(0, Math.min(100, (db + 60) / 60 * 100))}%`;
      level.className = db > -1 ? 'hot' : '';
      $('load').textContent = `load ${(msg.load * 100).toFixed(0)}%${msg.blown ? ' — blew up, reset' : ''}`;
      $('load').className = msg.load > 0.8 || msg.blown ? 'bad' : '';
    } else if (msg.type === 'file') {
      doc.path = msg.path;
      doc.dirty = msg.dirty;
      showFile();
      if (!sel) renderInspector();
    } else if (msg.type === 'ask') {
      // In a browser there is no file dialog: ask here, then send the
      // action again with what was missing.
      if (msg.dirty && !confirm('The scene has changes that are not saved. Discard them?')) return;
      if (msg.what === 'new') return send({ type: 'new', force: true });
      const path = prompt(msg.what === 'open' ? 'Scene file to open:' : 'Save the scene as:', msg.path || 'phys-scene.json');
      if (path) send({ type: msg.what, path, force: true });
    } else if (msg.type === 'shot') {
      // An agent asked to see the viewport.
      renderer.render(scene3, camera);
      send({ type: 'shot', data: renderer.domElement.toDataURL('image/jpeg', 0.85) });
    } else if (msg.type === 'take') {
      $('clear').textContent = msg.count ? `Clear take (${msg.count})` : 'Clear take';
    } else if (msg.type === 'note') {
      toast(msg.message);
    } else if (msg.type === 'error') {
      toast(msg.message, true);
      renderInspector();   // put rejected values back
    }
  };
}

// ---------------------------------------------------------------- wiring

for (const b of $('tools').children) b.addEventListener('click', () => setTool(b.dataset.tool));
for (const b of $('gizmo').children) b.addEventListener('click', () => { gizmo.setMode(b.dataset.mode); syncButtons(); });
$('add').addEventListener('change', (e) => { if (e.target.value) add(e.target.value); e.target.value = ''; });
$('undo').addEventListener('click', () => send({ type: 'undo' }));
$('redo').addEventListener('click', () => send({ type: 'redo' }));
$('save').addEventListener('click', () => send({ type: 'save' }));
$('saveas').addEventListener('click', () => send({ type: 'save_as' }));
$('open').addEventListener('click', () => send({ type: 'open' }));
$('new').addEventListener('click', () => send({ type: 'new' }));
$('render').addEventListener('click', () => { toast('rendering…'); send({ type: 'render' }); });
$('clear').addEventListener('click', () => send({ type: 'clear_take' }));
$('volume').addEventListener('input', (e) => send({ type: 'volume', value: Number(e.target.value) }));

function malletLabels() {
  $('speed_out').textContent = `${Number($('speed').value).toFixed(1)} m/s`;
  const h = Number($('hard').value);
  $('hard_out').textContent = h < 6.5 ? 'felt' : h < 7.5 ? 'rubber' : h < 8.5 ? 'wood' : h < 9.5 ? 'hard plastic' : 'steel';
  $('mass_out').textContent = `${(Math.pow(10, Number($('mass').value)) * 1000).toFixed(0)} g`;
  $('pull_out').textContent = `${Number($('pull').value).toFixed(1)} mm`;
  $('bowspeed_out').textContent = `${Number($('bowspeed').value).toFixed(2)} m/s`;
  $('pressure_out').textContent = `${Math.pow(10, Number($('pressure').value)).toFixed(2)} N`;
}
for (const id of ['speed', 'hard', 'mass', 'pull', 'bowspeed', 'pressure']) {
  $(id).addEventListener('input', () => {
    malletLabels();
    // Moving a bow slider while bowing changes the stroke under way.
    if (bowing) send({ type: 'strike', ...bowing, ...mallet() });
  });
}
malletLabels();
for (const b of $('hand').children) b.addEventListener('click', () => setHand(b.dataset.hand));
setHand('strike');

window.addEventListener('keydown', (e) => {
  if (e.target.matches('input, select, textarea')) return;
  const mod = e.metaKey || e.ctrlKey;
  if (mod && e.key.toLowerCase() === 'z') { send({ type: e.shiftKey ? 'redo' : 'undo' }); e.preventDefault(); }
  else if (mod && e.key.toLowerCase() === 's') { send({ type: 'save' }); e.preventDefault(); }
  else if (mod) return;
  else if (e.key === 'Delete' || e.key === 'Backspace') remove();
  else if (e.key === 'Escape') { jointFrom = null; sel = null; applySelection(); renderInspector(); }
  else if (e.key === 's') setTool('select');
  else if (e.key === 'h') setTool('strike');
  else if (e.key === 'j') setTool('joint');
  else if (e.key === 'p') setTool('pickup');
  else if (e.key === 'w') { gizmo.setMode('translate'); syncButtons(); }
  else if (e.key === 'e') { gizmo.setMode('rotate'); syncButtons(); }
});

let last = performance.now();
function frame(now) {
  const dt = Math.min(0.1, (now - last) / 1000);
  last = now;
  orbit.update();
  if (doc) animateVibration(dt);
  for (const m of [...marks.children]) {
    const age = (now - m.userData.born) / 600;
    if (age >= 1 && !(jointFrom && m === marks.children[marks.children.length - 1])) {
      marks.remove(m);
      dispose(m);
    } else {
      m.material.opacity = Math.max(0.25, 1 - age);
      m.scale.setScalar(1 + Math.min(age, 1) * 1.5);
    }
  }
  renderer.render(scene3, camera);
  requestAnimationFrame(frame);
}

setTool('select');
resize();
connect();
requestAnimationFrame(frame);

// For driving the page from a console or a test.
window.phys = { send, world, animateVibration, setHand, get doc() { return doc; }, get amps() { return amps; }, setTool, select: (kind, name) => { sel = { kind, name }; applySelection(); renderInspector(); } };
