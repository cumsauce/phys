// What the page knows, shared by every module, and a few helpers on it.

export const S = {
  doc: null,        // the last "scene" message from the backend
  sel: [],          // selected items: [{kind: 'part' | 'joint' | 'exciter' | 'listener', name}]
  tool: 'select',   // select | play | one of DRAW | joint | pickup
  back: 'select',   // the tool Tab returns to from Play
  hand: 'strike',   // how the hand excites: strike | pluck | bow
  amps: [],         // latest mode amplitudes, in display order
  jointFrom: null,  // first port of a joint being made by two clicks
  capture: null,    // the play point waiting for a key to be bound to it
  midi: [],         // connected MIDI inputs
  learn: null,      // the play point waiting for a MIDI note
  alt: false,       // Alt held: no snapping
  sentAt: 0,        // when this page last sent an edit
};

/** The tools that make something where you draw it. */
export const DRAW = ['string', 'bar', 'plate', 'membrane', 'mass', 'mic'];
export const isDraw = (tool) => DRAW.includes(tool);

/** Choices that outlive the page. */
export const prefs = { vibes: true, notes: true, hear: true, autotie: true };
try { Object.assign(prefs, JSON.parse(localStorage.getItem('phys.prefs') || '{}')); } catch { /* no storage */ }
export function setPref(key, value) {
  prefs[key] = value;
  try { localStorage.setItem('phys.prefs', JSON.stringify(prefs)); } catch { /* no storage */ }
  hooks.prefs?.(key);
}

/** Functions modules offer each other without importing in circles. */
export const hooks = {};

export const $ = (id) => document.getElementById(id);
export const deg = (r) => r * 180 / Math.PI;
export const rad = (d) => d * Math.PI / 180;
export const round = (x, p = 1e5) => Math.round(x * p) / p;
export const clone = (x) => structuredClone(x);

export const MATERIALS = ['steel', 'aluminium', 'brass', 'bronze', 'glass', 'wood', 'hardwood', 'stone', 'ice', 'plastic', 'nylon', 'mylar'];
export const COLORS = {
  steel: 0x9aa4b0, aluminium: 0xc4cad2, brass: 0xc9a54a, bronze: 0xb07a3c, glass: 0x9fd3de,
  wood: 0xc79a62, hardwood: 0x8a5a36, stone: 0x8d8d86, ice: 0xcfe8f5, plastic: 0xd8d2c0,
  nylon: 0xe8e4d8, mylar: 0xe9e9ef, metal: 0x9aa4b0, aluminum: 0xc4cad2,
};

export function listOf(kind) {
  const s = S.doc?.scene;
  if (!s) return [];
  return { part: s.parts, joint: s.joints, exciter: s.exciters, listener: s.listeners }[kind] ?? [];
}

export function findItem(sel) {
  return sel ? listOf(sel.kind).find((x) => x.name === sel.name) ?? null : null;
}

export const partByName = (name) => listOf('part').find((p) => p.name === name);

/** A name not yet used in `list`, nor in `taken` (names promised earlier in the same batch). */
export function freshName(base, list, taken = new Set()) {
  const used = new Set([...list.map((x) => x.name), ...taken]);
  const stem = base.replace(/\d+$/, '') || base;
  let name = base;
  for (let i = 2; used.has(name); i++) name = stem + i;
  taken.add(name);
  return name;
}

export const isSelected = (kind, name) => S.sel.some((s) => s.kind === kind && s.name === name);

/** Select `items`; with `add`, toggle them in the current selection. */
export function select(items, add = false) {
  if (!add) S.sel = items.map((i) => ({ kind: i.kind, name: i.name }));
  else {
    for (const i of items) {
      const at = S.sel.findIndex((s) => s.kind === i.kind && s.name === i.name);
      if (at >= 0) S.sel.splice(at, 1);
      else S.sel.push({ kind: i.kind, name: i.name });
    }
  }
  hooks.selection?.();
}

/** Drop selected names that no longer exist. */
export function pruneSelection() {
  S.sel = S.sel.filter((s) => findItem(s));
}

// ---------------------------------------------------------------- note names

const NAMES = ['C', 'C#', 'D', 'D#', 'E', 'F', 'F#', 'G', 'G#', 'A', 'A#', 'B'];

/** A frequency as the nearest note and how many cents off it is (A4 = 440 Hz). */
export function noteOf(hz) {
  if (!(hz > 0)) return null;
  const midi = 69 + 12 * Math.log2(hz / 440);
  const nearest = Math.round(midi);
  return { name: `${NAMES[((nearest % 12) + 12) % 12]}${Math.floor(nearest / 12) - 1}`, cents: Math.round((midi - nearest) * 100), midi: nearest };
}

/** "G3 +4c", or just "G3" when it is on the note. */
export function noteText(hz) {
  const n = noteOf(hz);
  if (!n) return '';
  return n.cents ? `${n.name} ${n.cents > 0 ? '+' : '−'}${Math.abs(n.cents)}c` : n.name;
}

/** A note name ("e2", "F#3", "Bb4", optionally with cents: "A4 +10") as a
 *  frequency, or null when it is not one. */
export function parseNote(text) {
  const m = /^\s*([a-gA-G])\s*([#b♯♭]?)\s*(-?\d)\s*(?:([+\-−]\s*\d+(?:\.\d+)?)\s*c?)?\s*$/.exec(text);
  if (!m) return null;
  const semis = { c: 0, d: 2, e: 4, f: 5, g: 7, a: 9, b: 11 }[m[1].toLowerCase()] + ({ '#': 1, '♯': 1, b: -1, '♭': -1 }[m[2]] ?? 0);
  const midi = 12 * (Number(m[3]) + 1) + semis;
  const cents = m[4] ? Number(m[4].replace('−', '-').replace(/\s/g, '')) : 0;
  return 440 * Math.pow(2, (midi - 69 + cents / 100) / 12);
}
