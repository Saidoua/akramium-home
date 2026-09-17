// The drive page: the files view and the trash view share this file. State comes from
// /api/drive; the folder id (or "trash") lives in the URL.

const $ = (id) => document.getElementById(id);
const files = $('files');
const crumbs = $('crumbs');
const empty = $('empty');
const uploads = $('uploads');
const drop = $('drop');
const menu = $('menu');
const toast = $('toast');
const rowTemplate = $('row');
const fileInput = $('file-input');

const view = location.pathname.endsWith('/drive/trash') ? 'trash' : location.pathname.endsWith('/drive/shared') ? 'shared' : 'files';
const folderId = () => {
  const m = location.pathname.match(/\/drive\/folder\/(\d+)/);
  return m ? Number(m[1]) : null;
};

let entries = [];
let menuTarget = null;

async function api(path, options = {}) {
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

function formatSize(n) {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < units.length - 1);
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

function formatWhen(seconds) {
  const d = new Date(seconds * 1000);
  const sameDay = d.toDateString() === new Date().toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString();
}

const THUMB_TYPES = ['image/jpeg', 'image/png', 'image/webp', 'image/gif'];
const TEXT_LIMIT = 512 * 1024;

function previewKind(entry) {
  if (entry.is_dir) return null;
  const mime = entry.mime || '';
  if (mime.startsWith('image/') && mime !== 'image/svg+xml') return 'image';
  if (['text/plain', 'text/markdown', 'text/csv', 'application/json'].includes(mime) || mime.startsWith('text/x-')) return 'text';
  return null;
}

function iconFor(entry) {
  if (entry.is_dir) return '#folder';
  if ((entry.mime || '').startsWith('image/')) return '#image';
  return '#file';
}

let toastTimer;
function say(message) {
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { toast.hidden = true; }, 3000);
}

function renderCrumbs(listing) {
  crumbs.replaceChildren();
  const root = document.createElement('a');
  root.href = '/drive/';
  root.textContent = 'My files';
  if (!listing.folder) root.className = 'here';
  crumbs.append(root);
  listing.crumbs.forEach((c, i) => {
    const sep = document.createElement('span');
    sep.className = 'sep';
    sep.textContent = '/';
    const a = document.createElement('a');
    a.href = `/drive/folder/${c.id}`;
    a.textContent = c.name;
    if (i === listing.crumbs.length - 1) a.className = 'here';
    crumbs.append(sep, a);
  });
}

function renderRows(list) {
  files.replaceChildren();
  for (const entry of list) {
    const node = rowTemplate.content.firstElementChild.cloneNode(true);
    node.dataset.id = entry.id;
    node.classList.toggle('dir', entry.is_dir);
    node.classList.toggle('trashed', view === 'trash');
    const name = node.querySelector('.name');
    name.textContent = entry.name;
    if (view === 'shared') {
      name.href = `/s/${entry.token}`;
      name.target = '_blank';
    } else if (view === 'files') {
      name.href = entry.is_dir ? `/drive/folder/${entry.id}` : `/api/drive/files/${entry.id}/content`;
      if (!entry.is_dir) name.target = '_blank';
    } else {
      name.removeAttribute('href');
    }
    node.querySelector('use').setAttribute('href', `/drive/icons.svg${iconFor(entry)}`);
    if (view === 'files' && THUMB_TYPES.includes(entry.mime) && entry.hash) {
      const img = new Image();
      img.loading = 'lazy';
      img.alt = '';
      img.src = `/api/drive/files/${entry.id}/thumb?h=${entry.hash.slice(0, 16)}`;
      // In the page from the start (a detached lazy image never loads), shown once it has loaded.
      img.onload = () => { img.classList.add('ready'); node.querySelector('.icon').remove(); };
      img.onerror = () => img.remove();
      node.querySelector('.lead').append(img);
    }
    if (view === 'files' && previewKind(entry)) {
      name.addEventListener('click', (e) => { if (!e.metaKey && !e.ctrlKey) { e.preventDefault(); openViewer(entry); } });
    }
    node.querySelector('.size').textContent = entry.is_dir || view === 'shared' ? '' : formatSize(entry.size);
    node.querySelector('.when').textContent = view === 'trash' ? `deleted ${formatWhen(entry.trashed_at)}`
      : view === 'shared' ? expiryText(entry.expires_at) : formatWhen(entry.mtime);
    node.querySelector('.more').addEventListener('click', (e) => openMenu(e, entry));
    node.addEventListener('contextmenu', (e) => openMenu(e, entry));
    node.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && view === 'files') name.click();
      if (view === 'files' && (e.key === 'Delete' || e.key === 'Backspace')) { e.preventDefault(); act('trash', entry); }
    });
    files.append(node);
  }
  empty.hidden = list.length > 0;
}

function expiryText(expires) {
  if (!expires) return 'no expiry';
  const left = expires - Date.now() / 1000;
  if (left <= 0) return 'expired';
  if (left >= 2 * 86400) return `${Math.floor(left / 86400)} days left`;
  if (left >= 2 * 3600) return `${Math.floor(left / 3600)} hours left`;
  return `${Math.max(1, Math.floor(left / 60))} min left`;
}

async function load() {
  if (view === 'shared') {
    const shares = await api('/api/drive/shares');
    entries = shares.map((s) => ({ ...s, share_id: s.id, id: s.file_id, size: 0, mime: '' }));
    crumbs.replaceChildren(Object.assign(document.createElement('span'), { className: 'here', textContent: 'Shared links' }));
    $('empty-title').textContent = 'Nothing is shared';
    $('empty-text').textContent = 'Use Share… on a file or folder to make a link.';
    renderRows(entries);
    return;
  }
  if (view === 'trash') {
    entries = await api('/api/drive/trash');
    crumbs.replaceChildren(Object.assign(document.createElement('span'), { className: 'here', textContent: 'Trash' }));
    $('empty-title').textContent = 'The trash is empty';
    $('empty-text').textContent = 'Deleted items wait here for 30 days, then go for good.';
    renderRows(entries);
    return;
  }
  const id = folderId();
  const listing = await api(`/api/drive/files${id ? `?folder=${id}` : ''}`);
  entries = listing.entries;
  renderCrumbs(listing);
  renderRows(entries);
}

// Actions menu
function openMenu(event, entry) {
  event.preventDefault();
  event.stopPropagation();
  menuTarget = entry;
  const only = { trash: ['restore', 'purge'], shared: ['copy', 'revoke'] }[view];
  for (const b of menu.querySelectorAll('button')) {
    const a = b.dataset.action;
    b.hidden = only ? !only.includes(a)
      : ['restore', 'purge', 'copy', 'revoke'].includes(a) || (a === 'download' && entry.is_dir) || (a === 'open' && !entry.is_dir);
  }
  menu.hidden = false;
  const x = Math.min(event.clientX, window.innerWidth - menu.offsetWidth - 8);
  const y = Math.min(event.clientY, window.innerHeight - menu.offsetHeight - 8);
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
  menu.querySelector('button:not([hidden])').focus();
}
function closeMenu() { menu.hidden = true; menuTarget = null; }
document.addEventListener('click', closeMenu);
document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeMenu(); });
menu.addEventListener('click', (e) => {
  const action = e.target.closest('button')?.dataset.action;
  const entry = menuTarget;
  closeMenu();
  if (action && entry) act(action, entry);
});

// Small prompt dialog
function ask(title, value = '') {
  const dialog = $('prompt');
  const input = $('prompt-input');
  $('prompt-title').textContent = title;
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

// Folder picker for Move
function pickFolder(moving) {
  const dialog = $('picker');
  const list = $('picker-list');
  const pcrumbs = $('picker-crumbs');
  let current = null;

  async function show(folder) {
    current = folder;
    const listing = await api(`/api/drive/files${folder ? `?folder=${folder}` : ''}`);
    pcrumbs.replaceChildren();
    const root = document.createElement('a');
    root.href = '#';
    root.textContent = 'My files';
    root.onclick = (e) => { e.preventDefault(); show(null); };
    if (!folder) root.className = 'here';
    pcrumbs.append(root);
    for (const c of listing.crumbs) {
      pcrumbs.append(Object.assign(document.createElement('span'), { className: 'sep', textContent: '/' }));
      const a = document.createElement('a');
      a.href = '#';
      a.textContent = c.name;
      a.onclick = (e) => { e.preventDefault(); show(c.id); };
      if (c.id === folder) a.className = 'here';
      pcrumbs.append(a);
    }
    list.replaceChildren();
    const folders = listing.entries.filter((e) => e.is_dir);
    if (!folders.length) list.append(Object.assign(document.createElement('div'), { className: 'none', textContent: 'No folders here' }));
    for (const f of folders) {
      const b = document.createElement('button');
      b.type = 'button';
      b.innerHTML = '<svg aria-hidden="true"><use href="/drive/icons.svg#folder"></use></svg><span></span>';
      b.querySelector('span').textContent = f.name;
      b.disabled = f.id === moving.id;
      b.onclick = () => show(f.id);
      list.append(b);
    }
    $('picker-ok').disabled = current === moving.parent_id;
  }

  return new Promise((resolve) => {
    $('picker-cancel').onclick = () => { dialog.close(); resolve(undefined); };
    $('picker-ok').onclick = () => { dialog.close(); resolve(current); };
    show(moving.parent_id ?? null).then(() => dialog.showModal());
  });
}

// Sharing
const shareUrl = (token) => `${location.origin}/s/${token}`;

async function copyText(text, input) {
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    // Plain http on the LAN is not a secure context: fall back to selecting and copying.
    const field = input || Object.assign(document.body.appendChild(document.createElement('input')), { value: text });
    field.select();
    document.execCommand('copy');
    if (!input) field.remove();
  }
  say('Link copied');
}

async function shareDialog(entry) {
  const dialog = $('share');
  const list = $('share-links');
  $('share-title').textContent = `Share ${entry.name}`;

  async function refresh() {
    const shares = await api(`/api/drive/files/${entry.id}/shares`);
    list.replaceChildren();
    for (const s of shares) {
      const item = document.createElement('div');
      item.className = 'share-link';
      const input = Object.assign(document.createElement('input'), { readOnly: true, value: shareUrl(s.token) });
      input.onfocus = () => input.select();
      const copy = Object.assign(document.createElement('button'), { type: 'button', className: 'ghost', textContent: 'Copy' });
      copy.onclick = () => copyText(input.value, input);
      const stop = Object.assign(document.createElement('button'), { type: 'button', className: 'ghost danger', textContent: 'Stop' });
      stop.onclick = async () => { await api(`/api/drive/shares/${s.id}`, { method: 'DELETE' }); await refresh(); };
      item.append(input, copy, stop, Object.assign(document.createElement('small'), { textContent: expiryText(s.expires_at) }));
      list.append(item);
    }
  }

  $('share-create').onclick = async () => {
    const value = $('share-expiry').value;
    try {
      await api(`/api/drive/files/${entry.id}/shares`, { method: 'POST', body: { expires_in: value ? Number(value) : null } });
      await refresh();
    } catch (e) {
      say(e.message);
    }
  };
  $('share-close').onclick = () => dialog.close();
  await refresh();
  dialog.showModal();
}

async function act(action, entry) {
  try {
    switch (action) {
      case 'open': location.href = `/drive/folder/${entry.id}`; return;
      case 'download': location.href = `/api/drive/files/${entry.id}/content?download=1`; return;
      case 'rename': {
        const name = await ask('Rename', entry.name);
        if (name && name !== entry.name) { await api(`/api/drive/files/${entry.id}/rename`, { method: 'POST', body: { name } }); await load(); }
        return;
      }
      case 'move': {
        const parent = await pickFolder(entry);
        if (parent !== undefined) { await api(`/api/drive/files/${entry.id}/move`, { method: 'POST', body: { parent } }); await load(); say(`Moved ${entry.name}`); }
        return;
      }
      case 'share': await shareDialog(entry); return;
      case 'copy': await copyText(shareUrl(entry.token)); return;
      case 'revoke':
        await api(`/api/drive/shares/${entry.share_id}`, { method: 'DELETE' });
        await load();
        say('Link stopped');
        return;
      case 'trash':
        await api(`/api/drive/files/${entry.id}`, { method: 'DELETE' });
        await load();
        say(`${entry.name} moved to trash`);
        return;
      case 'restore':
        await api(`/api/drive/trash/${entry.id}/restore`, { method: 'POST' });
        await load();
        say(`Restored ${entry.name}`);
        return;
      case 'purge':
        if (!confirm(`Delete ${entry.name} for good? This cannot be undone.`)) return;
        await api(`/api/drive/trash/${entry.id}`, { method: 'DELETE' });
        await load();
        return;
      default:
    }
  } catch (e) {
    say(e.message);
  }
}

// Preview overlay: images and text. Everything else opens in its own tab.
const viewer = $('viewer');
let viewerIndex = -1;
const previewable = () => entries.filter(previewKind);

async function openViewer(entry) {
  const list = previewable();
  viewerIndex = list.findIndex((e) => e.id === entry.id);
  const stage = $('viewer-stage');
  const url = `/api/drive/files/${entry.id}/content`;
  $('viewer-name').textContent = entry.name;
  $('viewer-download').href = `${url}?download=1`;
  $('viewer-prev').disabled = viewerIndex <= 0;
  $('viewer-next').disabled = viewerIndex >= list.length - 1;
  stage.replaceChildren();
  viewer.hidden = false;
  if (previewKind(entry) === 'image') {
    const img = new Image();
    img.alt = entry.name;
    img.src = url;
    stage.append(img);
  } else {
    const pre = document.createElement('pre');
    stage.append(pre);
    try {
      const response = await fetch(url, { headers: { range: `bytes=0-${TEXT_LIMIT - 1}` } });
      pre.textContent = await response.text();
      if (entry.size > TEXT_LIMIT) pre.textContent += `\n\n… showing the first ${formatSize(TEXT_LIMIT)} of ${formatSize(entry.size)}`;
    } catch (e) {
      stage.replaceChildren(Object.assign(document.createElement('div'), { className: 'note', textContent: 'Could not load the preview.' }));
    }
  }
  $('viewer-close').focus();
}
function closeViewer() { viewer.hidden = true; $('viewer-stage').replaceChildren(); viewerIndex = -1; }
function stepViewer(delta) {
  const list = previewable();
  const next = list[viewerIndex + delta];
  if (next) openViewer(next);
}
$('viewer-close').addEventListener('click', closeViewer);
$('viewer-prev').addEventListener('click', () => stepViewer(-1));
$('viewer-next').addEventListener('click', () => stepViewer(1));
viewer.addEventListener('click', (e) => { if (e.target === viewer || e.target.id === 'viewer-stage') closeViewer(); });
document.addEventListener('keydown', (e) => {
  if (viewer.hidden) return;
  if (e.key === 'Escape') closeViewer();
  if (e.key === 'ArrowLeft') stepViewer(-1);
  if (e.key === 'ArrowRight') stepViewer(1);
});

// Uploads: chunks at known offsets, so a lost connection or a closed tab costs one chunk.
// The upload id is remembered per file (name, size, date, folder); choosing the same file
// again carries on where it stopped.
const resumeKey = (file) => `dekave-upload:${folderId() ?? 0}:${file.name}:${file.size}:${file.lastModified}`;

async function openUpload(file) {
  const key = resumeKey(file);
  const known = localStorage.getItem(key);
  if (known) {
    const response = await fetch(`/api/drive/uploads/${known}`);
    if (response.ok) return response.json();
    localStorage.removeItem(key);
  }
  const up = await api('/api/drive/uploads', { method: 'POST', body: { parent: folderId(), name: file.name, size: file.size } });
  localStorage.setItem(key, up.id);
  return up;
}

async function uploadOne(file) {
  const card = document.createElement('div');
  card.className = 'up';
  card.innerHTML = '<span class="name"></span><span><span class="state"></span><button class="cancel" title="Cancel">✕</button></span><div class="bar-track"><div class="bar-fill"></div></div>';
  card.querySelector('.name').textContent = file.name;
  const state = card.querySelector('.state');
  const fill = card.querySelector('.bar-fill');
  uploads.append(card);
  uploads.hidden = false;
  let cancelled = false;
  let upId = null;
  card.querySelector('.cancel').onclick = () => { cancelled = true; };
  const show = (received) => {
    fill.style.width = `${file.size ? (received / file.size) * 100 : 100}%`;
    state.textContent = `${formatSize(received)} of ${formatSize(file.size)}`;
  };

  try {
    let up = await openUpload(file);
    upId = up.id;
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
    localStorage.removeItem(resumeKey(file));
    card.remove();
  } catch (e) {
    if (cancelled && upId) {
      await fetch(`/api/drive/uploads/${upId}`, { method: 'DELETE' }).catch(() => {});
      localStorage.removeItem(resumeKey(file));
      card.remove();
    } else {
      card.classList.add('failed');
      state.textContent = e.message;
      card.querySelector('.cancel').onclick = () => card.remove();
    }
  }
}

async function uploadAll(list) {
  for (const file of list) await uploadOne(file);
  if (!uploads.querySelector('.up')) uploads.hidden = true;
  await load();
}

// Wiring
$('nav-files').classList.toggle('current', view === 'files');
$('nav-trash').classList.toggle('current', view === 'trash');
$('nav-shared').classList.toggle('current', view === 'shared');
$('files-actions').hidden = view !== 'files';
$('trash-actions').hidden = view !== 'trash';

$('upload').addEventListener('click', () => fileInput.click());
fileInput.addEventListener('change', () => { uploadAll([...fileInput.files]); fileInput.value = ''; });
$('new-folder').addEventListener('click', async () => {
  const name = await ask('New folder');
  if (!name) return;
  try {
    await api('/api/drive/folders', { method: 'POST', body: { parent: folderId(), name } });
    await load();
  } catch (e) {
    say(e.message);
  }
});
$('empty-trash').addEventListener('click', async () => {
  if (!entries.length || !confirm('Delete everything in the trash for good?')) return;
  try {
    const r = await api('/api/drive/trash', { method: 'DELETE' });
    await load();
    say(`Deleted ${r.purged} item${r.purged === 1 ? '' : 's'}`);
  } catch (e) {
    say(e.message);
  }
});
$('signout').addEventListener('click', async () => {
  await fetch('/api/logout', { method: 'POST' });
  location.href = '/login';
});

if (view === 'files') {
  let dragDepth = 0;
  document.addEventListener('dragenter', (e) => { e.preventDefault(); dragDepth++; drop.hidden = false; });
  document.addEventListener('dragover', (e) => e.preventDefault());
  document.addEventListener('dragleave', () => { dragDepth = Math.max(0, dragDepth - 1); if (dragDepth === 0) drop.hidden = true; });
  document.addEventListener('drop', (e) => {
    e.preventDefault();
    dragDepth = 0;
    drop.hidden = true;
    if (e.dataTransfer.files.length) uploadAll([...e.dataTransfer.files]);
  });
}

api('/api/me').then((me) => { $('me-name').textContent = me.name; $('nav-people').hidden = !me.is_admin; });
load().catch((e) => { empty.hidden = false; $('empty-title').textContent = e.message; });
