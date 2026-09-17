// Preview overlay: images and text. Everything else opens in its own tab.

import { $, el, formatSize, previewKind } from '/drive/lib.js';

const TEXT_LIMIT = 512 * 1024;
const viewer = $('viewer');
let list = [];
let index = -1;
let returnFocus = null;

async function show(entry) {
  const stage = $('viewer-stage');
  const url = `/api/drive/files/${entry.id}/content`;
  $('viewer-name').textContent = entry.name;
  $('viewer-count').textContent = list.length > 1 ? `${index + 1} of ${list.length}` : '';
  $('viewer-download').href = `${url}?download=1`;
  $('viewer-prev').disabled = index <= 0;
  $('viewer-next').disabled = index >= list.length - 1;
  stage.replaceChildren();
  if (previewKind(entry) === 'image') {
    stage.append(el('img', { alt: entry.name, src: url }));
    return;
  }
  const pre = el('pre');
  stage.append(pre);
  try {
    const response = await fetch(url, { headers: { range: `bytes=0-${TEXT_LIMIT - 1}` } });
    pre.textContent = await response.text();
    if (entry.size > TEXT_LIMIT) pre.textContent += `\n\n… showing the first ${formatSize(TEXT_LIMIT)} of ${formatSize(entry.size)}`;
  } catch (e) {
    stage.replaceChildren(el('div', { className: 'note', textContent: 'Could not load the preview.' }));
  }
}

/** Opens the overlay on `entry`, with the arrows walking through `entries` that can preview. */
export function openViewer(entry, entries) {
  list = entries.filter(previewKind);
  index = list.findIndex((e) => e.id === entry.id);
  if (index < 0) return;
  returnFocus = document.activeElement;
  viewer.hidden = false;
  show(list[index]);
  $('viewer-close').focus();
}

export const viewerOpen = () => !viewer.hidden;

function close() {
  viewer.hidden = true;
  $('viewer-stage').replaceChildren();
  index = -1;
  if (returnFocus && returnFocus.isConnected) returnFocus.focus();
}

function step(delta) {
  const next = list[index + delta];
  if (!next) return;
  index += delta;
  show(next);
}

$('viewer-close').addEventListener('click', close);
$('viewer-prev').addEventListener('click', () => step(-1));
$('viewer-next').addEventListener('click', () => step(1));
viewer.addEventListener('click', (e) => { if (e.target === viewer || e.target.id === 'viewer-stage') close(); });
document.addEventListener('keydown', (e) => {
  if (viewer.hidden) return;
  if (e.key === 'Escape') close();
  else if (e.key === 'ArrowLeft') step(-1);
  else if (e.key === 'ArrowRight') step(1);
  else if (e.key === 'Tab') {
    // Keep the focus inside the overlay.
    const stops = [...viewer.querySelectorAll('a[href], button:not(:disabled)')];
    const at = stops.indexOf(document.activeElement);
    e.preventDefault();
    stops[(at + (e.shiftKey ? -1 : 1) + stops.length) % stops.length].focus();
  } else return;
  e.stopPropagation();
}, true);
