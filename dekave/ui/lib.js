// Shared pieces of the drive page: the API call, formatting, the toast, the two small dialogs.

export const $ = (id) => document.getElementById(id);

export const folderId = () => {
  const m = location.pathname.match(/\/drive\/folder\/(\d+)/);
  return m ? Number(m[1]) : null;
};

export async function api(path, options = {}) {
  if (options.body && typeof options.body !== 'string') {
    options.body = JSON.stringify(options.body);
    options.headers = { 'content-type': 'application/json', ...(options.headers || {}) };
  }
  const response = await fetch(path, options);
  if (response.status === 401) {
    location.href = `/login?next=${encodeURIComponent(location.pathname)}`;
    throw new Error('signed out');
  }
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw new Error(data.error || `Failed (${response.status})`);
  }
  return response.status === 204 ? null : response.json();
}

export function formatSize(n) {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < units.length - 1);
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

export function formatWhen(seconds) {
  const d = new Date(seconds * 1000);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString();
}

export function formatWhenLong(seconds) {
  return new Date(seconds * 1000).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
}

export function expiryText(expires) {
  if (!expires) return 'no expiry';
  const left = expires - Date.now() / 1000;
  if (left <= 0) return 'expired';
  if (left >= 2 * 86400) return `${Math.floor(left / 86400)} days left`;
  if (left >= 2 * 3600) return `${Math.floor(left / 3600)} hours left`;
  return `${Math.max(1, Math.floor(left / 60))} min left`;
}

export const THUMB_TYPES = ['image/jpeg', 'image/png', 'image/webp', 'image/gif'];

export function previewKind(entry) {
  if (entry.is_dir) return null;
  const mime = entry.mime || '';
  if (mime.startsWith('image/') && mime !== 'image/svg+xml') return 'image';
  if (['text/plain', 'text/markdown', 'text/csv', 'application/json'].includes(mime) || mime.startsWith('text/x-')) return 'text';
  return null;
}

export function iconFor(entry) {
  if (entry.is_dir) return 'folder';
  const mime = entry.mime || '';
  if (mime.startsWith('image/')) return 'image';
  if (mime.startsWith('video/')) return 'video';
  if (mime.startsWith('audio/')) return 'audio';
  if (mime === 'application/pdf') return 'pdf';
  if (mime.startsWith('text/') || mime === 'application/json') return 'text';
  if (/zip|compressed|x-tar|gzip|x-7z/.test(mime)) return 'archive';
  return 'file';
}

const KINDS = { folder: 'Folder', image: 'Image', video: 'Video', audio: 'Audio', pdf: 'PDF document', text: 'Text', archive: 'Archive', file: 'File' };
export function kindOf(entry) {
  const base = KINDS[iconFor(entry)];
  const dot = entry.name.lastIndexOf('.');
  const ext = !entry.is_dir && dot > 0 ? entry.name.slice(dot + 1).toUpperCase() : '';
  return ext && ext.length <= 5 && base !== 'PDF document' ? `${ext} ${base.toLowerCase()}` : base;
}

export function icon(name) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', `icon ${name}`);
  svg.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `/drive/icons.svg#${name}`);
  svg.append(use);
  return svg;
}

export function el(tag, props = {}, ...children) {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

let toastTimer;
export function say(message) {
  const toast = $('toast');
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { toast.hidden = true; }, 3200);
}

/** A one-field dialog. Resolves to the trimmed text, or null when cancelled. */
export function ask(title, value = '', okLabel = 'OK') {
  const dialog = $('prompt');
  const input = $('prompt-input');
  $('prompt-title').textContent = title;
  $('prompt-ok').textContent = okLabel;
  input.value = value;
  return new Promise((resolve) => {
    const done = () => { dialog.removeEventListener('close', done); resolve(dialog.returnValue === 'ok' ? input.value.trim() : null); };
    dialog.addEventListener('close', done);
    $('prompt-cancel').onclick = () => dialog.close('cancel');
    dialog.showModal();
    const dot = value.lastIndexOf('.');
    input.setSelectionRange(0, dot > 0 ? dot : value.length);
  });
}

/** A yes/no dialog for things that cannot be undone. */
export function confirmDanger(title, text, okLabel) {
  const dialog = $('confirm');
  $('confirm-title').textContent = title;
  $('confirm-text').textContent = text;
  $('confirm-ok').textContent = okLabel;
  return new Promise((resolve) => {
    const done = () => { dialog.removeEventListener('close', done); resolve(dialog.returnValue === 'ok'); };
    dialog.addEventListener('close', done);
    $('confirm-cancel').onclick = () => dialog.close('cancel');
    $('confirm-ok').onclick = () => dialog.close('ok');
    dialog.showModal();
    $('confirm-cancel').focus();
  });
}

/** Folder chooser for a move. `moving` are the entries being moved. Resolves to a folder id,
 *  null for the root, or undefined when cancelled. */
export function pickFolder(moving) {
  const dialog = $('picker');
  const list = $('picker-list');
  const pcrumbs = $('picker-crumbs');
  const movingIds = new Set(moving.map((e) => e.id));
  const from = moving[0].parent_id ?? null;
  let current = null;

  async function show(folder) {
    current = folder;
    const listing = await api(`/api/drive/files${folder ? `?folder=${folder}` : ''}`);
    pcrumbs.replaceChildren();
    const crumb = (label, id, here) => {
      const a = el('a', { href: '#', textContent: label, className: here ? 'here' : '' });
      a.onclick = (e) => { e.preventDefault(); show(id); };
      return a;
    };
    pcrumbs.append(crumb('My files', null, !folder));
    for (const c of listing.crumbs) pcrumbs.append(el('span', { className: 'sep', textContent: '/' }), crumb(c.name, c.id, c.id === folder));
    list.replaceChildren();
    const folders = listing.entries.filter((e) => e.is_dir);
    if (!folders.length) list.append(el('div', { className: 'none', textContent: 'No folders here' }));
    for (const f of folders) {
      const b = el('button', { type: 'button', disabled: movingIds.has(f.id) }, icon('folder'), el('span', { textContent: f.name }));
      b.onclick = () => show(f.id);
      list.append(b);
    }
    const insideMoving = listing.crumbs.some((c) => movingIds.has(c.id));
    $('picker-ok').disabled = current === from || insideMoving;
  }

  $('picker-title').textContent = moving.length === 1 ? `Move ${moving[0].name}` : `Move ${moving.length} items`;
  return new Promise((resolve) => {
    const done = () => { dialog.removeEventListener('close', done); resolve(dialog.returnValue === 'ok' ? current : undefined); };
    dialog.addEventListener('close', done);
    $('picker-cancel').onclick = () => dialog.close('cancel');
    $('picker-ok').onclick = () => dialog.close('ok');
    show(from).then(() => dialog.showModal());
  });
}

export async function copyText(text, input) {
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    // Plain http on the LAN is not a secure context: fall back to selecting and copying.
    const field = input || document.body.appendChild(el('input', { value: text }));
    field.select();
    document.execCommand('copy');
    if (!input) field.remove();
  }
  say('Link copied');
}
