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

const view = location.pathname.endsWith('/drive/trash') ? 'trash' : 'files';
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
    if (view === 'files') {
      name.href = entry.is_dir ? `/drive/folder/${entry.id}` : `/api/drive/files/${entry.id}/content`;
      if (!entry.is_dir) name.target = '_blank';
    } else {
      name.removeAttribute('href');
    }
    node.querySelector('use').setAttribute('href', `/drive/icons.svg${iconFor(entry)}`);
    node.querySelector('.size').textContent = entry.is_dir ? '' : formatSize(entry.size);
    node.querySelector('.when').textContent = view === 'trash' ? `deleted ${formatWhen(entry.trashed_at)}` : formatWhen(entry.mtime);
    node.querySelector('.more').addEventListener('click', (e) => openMenu(e, entry));
    node.addEventListener('contextmenu', (e) => openMenu(e, entry));
    node.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && view === 'files') name.click();
      if (e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); act('trash', entry); }
    });
    files.append(node);
  }
  empty.hidden = list.length > 0;
}

async function load() {
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
  const inTrash = view === 'trash';
  for (const b of menu.querySelectorAll('button')) {
    const a = b.dataset.action;
    b.hidden = inTrash ? !['restore', 'purge'].includes(a)
      : ['restore', 'purge'].includes(a) || (a === 'download' && entry.is_dir) || (a === 'open' && !entry.is_dir);
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

// Uploads
function uploadOne(file) {
  const card = document.createElement('div');
  card.className = 'up';
  card.innerHTML = '<span class="name"></span><span class="state"></span><div class="bar-track"><div class="bar-fill"></div></div>';
  card.querySelector('.name').textContent = file.name;
  const state = card.querySelector('.state');
  const fill = card.querySelector('.bar-fill');
  uploads.append(card);
  uploads.hidden = false;
  return new Promise((resolve) => {
    const xhr = new XMLHttpRequest();
    const query = new URLSearchParams({ name: file.name });
    if (folderId()) query.set('parent', folderId());
    xhr.open('PUT', `/api/drive/files?${query}`);
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) {
        fill.style.width = `${(e.loaded / e.total) * 100}%`;
        state.textContent = `${formatSize(e.loaded)} of ${formatSize(e.total)}`;
      }
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        card.remove();
      } else {
        let message = `Failed (${xhr.status})`;
        try { message = JSON.parse(xhr.responseText).error || message; } catch (e) { /* keep the status */ }
        card.classList.add('failed');
        state.textContent = message;
      }
      resolve();
    };
    xhr.onerror = () => { card.classList.add('failed'); state.textContent = 'Connection lost'; resolve(); };
    xhr.send(file);
  });
}

async function uploadAll(list) {
  for (const file of list) await uploadOne(file);
  if (!uploads.querySelector('.up')) uploads.hidden = true;
  await load();
}

// Wiring
$('nav-files').classList.toggle('current', view === 'files');
$('nav-trash').classList.toggle('current', view === 'trash');
$('files-actions').hidden = view === 'trash';
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

api('/api/me').then((me) => { $('me-name').textContent = me.name; });
load().catch((e) => { empty.hidden = false; $('empty-title').textContent = e.message; });
