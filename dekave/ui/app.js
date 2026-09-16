// The drive page. Everything comes from /api/drive; the folder id lives in the URL.

const files = document.getElementById('files');
const crumbs = document.getElementById('crumbs');
const empty = document.getElementById('empty');
const uploads = document.getElementById('uploads');
const drop = document.getElementById('drop');
const rowTemplate = document.getElementById('row');
const fileInput = document.getElementById('file-input');

const folderId = () => {
  const m = location.pathname.match(/\/drive\/folder\/(\d+)/);
  return m ? Number(m[1]) : null;
};

async function api(path, options = {}) {
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
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : d.toLocaleDateString();
}

function iconFor(entry) {
  if (entry.is_dir) return '#folder';
  if ((entry.mime || '').startsWith('image/')) return '#image';
  return '#file';
}

function render(listing) {
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

  files.replaceChildren();
  for (const entry of listing.entries) {
    const node = rowTemplate.content.firstElementChild.cloneNode(true);
    node.classList.toggle('dir', entry.is_dir);
    node.href = entry.is_dir ? `/drive/folder/${entry.id}` : `/api/drive/files/${entry.id}/content`;
    if (!entry.is_dir) node.target = '_blank';
    node.querySelector('use').setAttribute('href', `/drive/icons.svg${iconFor(entry)}`);
    node.querySelector('.name').textContent = entry.name;
    node.querySelector('.size').textContent = entry.is_dir ? '' : formatSize(entry.size);
    node.querySelector('.when').textContent = formatWhen(entry.mtime);
    files.append(node);
  }
  empty.hidden = listing.entries.length > 0;
}

async function load() {
  const id = folderId();
  const listing = await api(`/api/drive/files${id ? `?folder=${id}` : ''}`);
  render(listing);
}

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
    const id = folderId();
    const query = new URLSearchParams({ name: file.name });
    if (id) query.set('parent', id);
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

document.getElementById('upload').addEventListener('click', () => fileInput.click());
fileInput.addEventListener('change', () => { uploadAll([...fileInput.files]); fileInput.value = ''; });

document.getElementById('new-folder').addEventListener('click', async () => {
  const name = prompt('Folder name');
  if (!name) return;
  try {
    await api('/api/drive/folders', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ parent: folderId(), name: name.trim() }),
    });
    await load();
  } catch (e) {
    alert(e.message);
  }
});

document.getElementById('signout').addEventListener('click', async () => {
  await fetch('/api/logout', { method: 'POST' });
  location.href = '/login';
});

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

api('/api/me').then((me) => { document.getElementById('me-name').textContent = me.name; });
load().catch((e) => { empty.hidden = false; empty.querySelector('h2').textContent = e.message; });
