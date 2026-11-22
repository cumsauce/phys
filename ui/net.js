// The connection to the backend.

import { $ } from './state.js';

let ws = null;

let toastAt = 0;

export function toast(message, error = false) {
  const t = $('toast');
  t.textContent = message;
  t.className = error ? 'error' : '';
  toastAt = performance.now();
}

/** Clear the status line, unless it was written a moment ago: an edit's
 *  own message should outlive the scene update that edit causes. */
export function settle() {
  if (performance.now() - toastAt > 2500) toast('');
}

export function send(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
  else toast('not connected', true);
}

/** One batch of ops: one undo step. */
export const ops = (list) => { if (list.length) send({ type: 'ops', ops: list }); };

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
