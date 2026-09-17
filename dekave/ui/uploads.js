// Uploads: chunks at known offsets, so a lost connection or a closed tab costs one chunk.
// The upload id is remembered per file (name, size, date, folder); choosing the same file
// again carries on where it stopped.

import { $, api, el, folderId, formatSize, say } from '/drive/lib.js';

const resumeKey = (file, parent) => `dekave-upload:${parent ?? 0}:${file.name}:${file.size}:${file.lastModified}`;

async function open(file, parent) {
  const key = resumeKey(file, parent);
  const known = localStorage.getItem(key);
  if (known) {
    const response = await fetch(`/api/drive/uploads/${known}`);
    if (response.ok) return response.json();
    localStorage.removeItem(key);
  }
  const up = await api('/api/drive/uploads', { method: 'POST', body: { parent, name: file.name, size: file.size } });
  localStorage.setItem(key, up.id);
  return up;
}

async function one(file, parent) {
  const uploads = $('uploads');
  const state = el('span', { className: 'state' });
  const cancel = el('button', { className: 'cancel', title: 'Cancel', textContent: '✕' });
  const fill = el('div', { className: 'bar-fill' });
  const card = el('div', { className: 'up' }, el('span', { className: 'name', textContent: file.name }), el('span', {}, state, cancel), el('div', { className: 'bar-track' }, fill));
  uploads.append(card);
  uploads.hidden = false;

  let cancelled = false;
  let id = null;
  cancel.onclick = () => { cancelled = true; };
  const show = (received) => {
    fill.style.width = `${file.size ? (received / file.size) * 100 : 100}%`;
    state.textContent = `${formatSize(received)} of ${formatSize(file.size)}`;
  };

  try {
    let up = await open(file, parent);
    id = up.id;
    if (up.received > 0) say(`Resuming ${file.name}`);
    show(up.received);
    let failures = 0;
    while (up.received < file.size) {
      if (cancelled) throw new Error('cancelled');
      const chunk = file.slice(up.received, up.received + up.chunk);
      const response = await fetch(`/api/drive/uploads/${up.id}/${up.received}`, { method: 'PUT', body: chunk }).catch(() => null);
      if (response && response.ok) {
        up = await response.json();
        failures = 0;
        show(up.received);
        continue;
      }
      if (response && response.status !== 409 && response.status < 500) {
        const data = await response.json().catch(() => ({}));
        throw new Error(data.error || `Failed (${response.status})`);
      }
      // Lost connection, server hiccup, or an offset mismatch: ask what arrived and go on.
      if (++failures > 5) throw new Error('Connection lost');
      state.textContent = 'Reconnecting…';
      await new Promise((r) => setTimeout(r, 1000 * failures));
      const status = await fetch(`/api/drive/uploads/${up.id}`).catch(() => null);
      if (status && status.ok) up = await status.json();
    }
    await api(`/api/drive/uploads/${up.id}/finish`, { method: 'POST' });
    localStorage.removeItem(resumeKey(file, parent));
    card.remove();
    return true;
  } catch (e) {
    if (cancelled && id) {
      await fetch(`/api/drive/uploads/${id}`, { method: 'DELETE' }).catch(() => {});
      localStorage.removeItem(resumeKey(file, parent));
      card.remove();
    } else {
      card.classList.add('failed');
      state.textContent = e.message;
      cancel.onclick = () => { card.remove(); if (!uploads.querySelector('.up')) uploads.hidden = true; };
    }
    return false;
  }
}

/** Uploads the files into the folder shown now, one after another, then calls `done`. */
export async function uploadAll(list, done) {
  const parent = folderId();
  let ok = 0;
  for (const file of list) if (await one(file, parent)) ok++;
  const uploads = $('uploads');
  if (!uploads.querySelector('.up')) uploads.hidden = true;
  if (ok > 1) say(`Uploaded ${ok} files`);
  await done();
}
