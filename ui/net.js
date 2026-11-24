// The connection to the backend.

import { S, $ } from './state.js';

let ws = null;

let toastTimer = null;

/** A short message over the view; it fades by itself. */
export function toast(message, error = false) {
  const t = $('toast');
  clearTimeout(toastTimer);
  if (!message) { t.className = ''; return; }
  t.textContent = message;
  t.className = error ? 'show error' : 'show';
  toastTimer = setTimeout(() => { t.className = ''; }, error ? 6000 : 3500);
}

export function send(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
  else toast('not connected', true);
}

/** One batch of ops: one undo step. */
export const ops = (list) => {
  if (!list.length) return;
  S.sentAt = performance.now();
  send({ type: 'ops', ops: list });
};

/** Connect, and keep reconnecting. `handlers` maps a message type to a function. */
export function connect(handlers) {
  ws = new WebSocket(`ws://${location.host}/ws`);
  ws.onopen = () => {
    $('conn').textContent = 'connected';
    $('conn').className = 'good';
    send({ type: 'volume', value: Number($('volume').value) });
  };
  ws.onclose = () => {
    $('conn').textContent = 'disconnected, retrying…';
    $('conn').className = 'bad';
    setTimeout(() => connect(handlers), 1000);
  };
  ws.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    handlers[msg.type]?.(msg);
  };
}
