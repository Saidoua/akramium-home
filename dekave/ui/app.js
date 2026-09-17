// The drive page. Three views share it: files (a folder), shared links, trash. State comes
// from /api/drive; the folder id or the view lives in the URL.

import { $, api, ask, confirmDanger, copyText, el, expiryText, folderId, formatSize, formatWhen, formatWhenLong, icon, iconFor, kindOf, pickFolder, previewKind, say, THUMB_TYPES } from '/drive/lib.js';
import { uploadAll } from '/drive/uploads.js';
import { openViewer, viewerOpen } from '/drive/viewer.js';
import { shareDialog, shareUrl } from '/drive/share.js';

const view = location.pathname.endsWith('/drive/trash') ? 'trash' : location.pathname.endsWith('/drive/shared') ? 'shared' : 'files';
const selectable = view !== 'shared';

const list = $('files');
const menu = $('menu');
let entries = [];            // as shown, after sorting
let listing = null;          // the folder listing (files view)
const selected = new Set();  // entry ids
let anchor = -1;             // where a shift-range starts
let focused = -1;            // the row the keyboard is on
let menuTarget = null;

// ---- sorting -----------------------------------------------------------------------------

const SORT_KEY = 'dekave-sort';
let sort = { key: 'name', dir: 1 };
try { sort = { ...sort, ...JSON.parse(localStorage.getItem(SORT_KEY) || '{}') }; } catch (e) { /* keep the default */ }

function sortEntries(items) {
  const by = {
    name: (a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }),
    size: (a, b) => a.size - b.size,
    when: (a, b) => (a.trashed_at ?? a.expires_at ?? a.mtime ?? 0) - (b.trashed_at ?? b.expires_at ?? b.mtime ?? 0),
  }[sort.key];
  return [...items].sort((a, b) => (b.is_dir - a.is_dir) || by(a, b) * sort.dir || a.name.localeCompare(b.name));
}

function renderSortHeader() {
  for (const b of document.querySelectorAll('.head [data-sort]')) {
    const on = b.dataset.sort === sort.key;
    b.classList.toggle('on', on);
    b.setAttribute('aria-sort', on ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none');
    b.querySelector('.arrow').textContent = on ? (sort.dir === 1 ? '↑' : '↓') : '';
  }
}

// ---- rendering ---------------------------------------------------------------------------

function renderCrumbs() {
  const crumbs = $('crumbs');
  crumbs.replaceChildren();
  if (view !== 'files') {
    crumbs.append(el('span', { className: 'here', textContent: view === 'trash' ? 'Trash' : 'Shared links' }));
    return;
  }
  const link = (label, id, here) => {
    const a = el('a', { href: id ? `/drive/folder/${id}` : '/drive/', textContent: label, className: here ? 'here' : '' });
    a.dataset.folder = id ?? '';
    return a;
  };
  crumbs.append(link('My files', null, !listing.folder));
  listing.crumbs.forEach((c, i) => crumbs.append(el('span', { className: 'sep', textContent: '/' }), link(c.name, c.id, i === listing.crumbs.length - 1)));
}

function whenText(entry) {
  if (view === 'trash') return `deleted ${formatWhen(entry.trashed_at)}`;
  if (view === 'shared') return expiryText(entry.expires_at);
  return formatWhen(entry.mtime);
}

function hrefOf(entry) {
  if (view === 'shared') return `/s/${entry.token}`;
  if (view === 'trash') return null;
  return entry.is_dir ? `/drive/folder/${entry.id}` : `/api/drive/files/${entry.id}/content`;
}

function renderRows() {
  list.replaceChildren();
  entries.forEach((entry, i) => {
    const kind = iconFor(entry);
    const lead = el('span', { className: 'lead' }, icon(kind));
    if (view === 'files' && THUMB_TYPES.includes(entry.mime) && entry.hash) {
      // In the page from the start (a detached lazy image never loads), shown once loaded.
      const img = el('img', { loading: 'lazy', alt: '', src: `/api/drive/files/${entry.id}/thumb?h=${entry.hash.slice(0, 16)}` });
      img.onload = () => { img.classList.add('ready'); lead.querySelector('.icon')?.remove(); };
      img.onerror = () => img.remove();
      lead.append(img);
    }
    const name = el('a', { className: 'name', textContent: entry.name, tabIndex: -1 });
    const href = hrefOf(entry);
    if (href) name.href = href;
    if (href && (!entry.is_dir || view === 'shared')) name.target = '_blank';
    name.addEventListener('click', (e) => {
      if (e.metaKey || e.ctrlKey || e.shiftKey) { e.preventDefault(); return; }
      e.stopPropagation();
      if (view === 'files' && previewKind(entry)) { e.preventDefault(); openViewer(entry, entries); }
    });

    const check = el('span', { className: 'check', role: 'checkbox', ariaLabel: `Select ${entry.name}` });
    check.addEventListener('click', (e) => { e.stopPropagation(); toggle(i); });
    const more = el('button', { className: 'more', ariaLabel: `Actions for ${entry.name}`, tabIndex: -1, textContent: '···' });
    more.setAttribute('aria-haspopup', 'menu');
    more.addEventListener('click', (e) => openMenu(e, entry, i));

    const row = el('div', { className: `row${entry.is_dir ? ' dir' : ''}`, tabIndex: i === Math.max(focused, 0) ? 0 : -1 },
      selectable ? check : el('span'), lead, name,
      el('span', { className: 'size', textContent: entry.is_dir || view === 'shared' ? '' : formatSize(entry.size) }),
      el('span', { className: 'when', textContent: whenText(entry) }), more);
    row.dataset.index = i;
    row.setAttribute('role', 'option');
    row.addEventListener('click', (e) => rowClick(e, i));
    row.addEventListener('dblclick', () => open(entry));
    row.addEventListener('contextmenu', (e) => openMenu(e, entry, i));
    row.addEventListener('focus', () => { focused = i; });
    if (view === 'files') wireDrag(row, entry, i);
    list.append(row);
  });
  const none = entries.length === 0;
  $('empty').hidden = !none;
  $('head').hidden = none;
  paintSelection();
}

function paintSelection() {
  for (const row of list.children) {
    const on = selected.has(entries[row.dataset.index].id);
    row.classList.toggle('selected', on);
    row.setAttribute('aria-selected', on);
    row.querySelector('.check')?.setAttribute('aria-checked', on);
  }
  const n = selected.size;
  $('selection').hidden = n === 0;
  $('files-actions').hidden = view !== 'files' || n > 0;
  $('trash-actions').hidden = view !== 'trash' || n > 0;
  if (n > 0) {
    $('selection-count').textContent = n === 1 ? '1 selected' : `${n} selected`;
    const one = n === 1 ? chosen()[0] : null;
    for (const b of $('selection').querySelectorAll('[data-action]')) {
      const a = b.dataset.action;
      if (a === 'clear') continue;
      const forView = view === 'trash' ? ['restore', 'purge'].includes(a) : ['download', 'share', 'rename', 'move', 'trash'].includes(a);
      const forCount = !['download', 'share', 'rename'].includes(a) || (one && !(a === 'download' && one.is_dir));
      b.hidden = !(forView && forCount);
    }
  }
  const all = $('select-all');
  if (all) { all.setAttribute('aria-checked', n > 0 && n === entries.length ? 'true' : n > 0 ? 'mixed' : 'false'); }
  renderDetails();
}

const chosen = () => entries.filter((e) => selected.has(e.id));

// ---- details pane ------------------------------------------------------------------------

let detailsFor = null;
function renderDetails() {
  const pane = $('details');
  const one = view === 'files' && selected.size === 1 ? chosen()[0] : null;
  document.body.classList.toggle('with-details', !!one && localStorage.getItem('dekave-details') !== 'off');
  if (!one) { detailsFor = null; pane.replaceChildren(); return; }
  if (detailsFor === one.id) return;
  detailsFor = one.id;

  const art = el('div', { className: 'art' }, icon(iconFor(one)));
  if (THUMB_TYPES.includes(one.mime) && one.hash) {
    const img = el('img', { alt: '', src: `/api/drive/files/${one.id}/thumb?h=${one.hash.slice(0, 16)}` });
    img.onload = () => art.replaceChildren(img);
  }
  const fact = (label, value) => el('div', { className: 'fact' }, el('dt', { textContent: label }), el('dd', { textContent: value }));
  const where = listing.crumbs.length ? `My files / ${listing.crumbs.map((c) => c.name).join(' / ')}` : 'My files';
  const facts = el('dl', {}, fact('Kind', kindOf(one)), ...(one.is_dir ? [] : [fact('Size', `${formatSize(one.size)} (${one.size.toLocaleString()} bytes)`)]), fact('Changed', formatWhenLong(one.mtime)), fact('In', where));
  const links = el('div', { className: 'fact' }, el('dt', { textContent: 'Sharing' }), el('dd', { textContent: '…' }));
  facts.append(links);
  const actions = el('div', { className: 'details-actions' });
  const button = (label, action, cls = 'ghost') => { const b = el('button', { className: cls, textContent: label }); b.onclick = () => act(action, [one]); return b; };
  actions.append(button(one.is_dir ? 'Open' : previewKind(one) ? 'Preview' : 'Open', 'open', 'primary slim'), button('Share…', 'share'));
  if (!one.is_dir) actions.append(button('Download', 'download'));
  pane.replaceChildren(el('button', { className: 'details-close', ariaLabel: 'Hide details', textContent: '✕', onclick: () => { localStorage.setItem('dekave-details', 'off'); document.body.classList.remove('with-details'); } }),
    art, el('h2', { textContent: one.name }), facts, actions);

  api(`/api/drive/files/${one.id}/shares`).then((shares) => {
    if (detailsFor !== one.id) return;
    links.querySelector('dd').textContent = shares.length === 0 ? 'Not shared' : shares.length === 1 ? `1 link (${expiryText(shares[0].expires_at)})` : `${shares.length} links`;
  }).catch(() => {});
}

// ---- selection and keyboard ----------------------------------------------------------------

function selectOnly(i) { selected.clear(); selected.add(entries[i].id); anchor = i; }
function toggle(i) { const id = entries[i].id; if (!selected.delete(id)) selected.add(id); anchor = i; focusRow(i, false); paintSelection(); }
function selectRange(to) {
  const from = anchor < 0 ? to : anchor;
  selected.clear();
  for (let k = Math.min(from, to); k <= Math.max(from, to); k++) selected.add(entries[k].id);
}
function clearSelection() { selected.clear(); anchor = -1; paintSelection(); }

function focusRow(i, scroll = true) {
  if (i < 0 || i >= entries.length) return;
  for (const row of list.children) row.tabIndex = -1;
  const row = list.children[i];
  row.tabIndex = 0;
  focused = i;
  row.focus({ preventScroll: !scroll });
  if (scroll) row.scrollIntoView({ block: 'nearest' });
}

function rowClick(e, i) {
  if (e.target.closest('.more, .check')) return;
  if (!selectable) return;
  if (e.shiftKey) selectRange(i);
  else if (e.metaKey || e.ctrlKey) { toggle(i); return; }
  else selectOnly(i);
  if (localStorage.getItem('dekave-details') === 'off' && e.detail === 1) { /* the pane stays hidden until asked for */ }
  focusRow(i, false);
  paintSelection();
}

function open(entry) {
  if (view === 'trash') return;
  if (view === 'shared') { window.open(`/s/${entry.token}`, '_blank'); return; }
  if (entry.is_dir) location.href = `/drive/folder/${entry.id}`;
  else if (previewKind(entry)) openViewer(entry, entries);
  else window.open(`/api/drive/files/${entry.id}/content`, '_blank');
}

list.addEventListener('keydown', (e) => {
  if (viewerOpen() || !entries.length) return;
  const i = Math.max(focused, 0);
  const move = (to) => {
    e.preventDefault();
    const next = Math.min(entries.length - 1, Math.max(0, to));
    if (selectable && e.shiftKey) { selectRange(next); paintSelection(); }
    focusRow(next);
  };
  switch (e.key) {
    case 'ArrowDown': move(i + 1); break;
    case 'ArrowUp': move(i - 1); break;
    case 'Home': move(0); break;
    case 'End': move(entries.length - 1); break;
    case 'PageDown': move(i + 10); break;
    case 'PageUp': move(i - 10); break;
    case 'Enter': e.preventDefault(); open(entries[i]); break;
    case ' ':
    case 'Spacebar': if (selectable) { e.preventDefault(); toggle(i); } break;
    case 'F2': if (view === 'files') { e.preventDefault(); act('rename', [entries[i]]); } break;
    case 'Delete':
    case 'Backspace':
      if (selectable) { e.preventDefault(); act(view === 'trash' ? 'purge' : 'trash', selected.size ? chosen() : [entries[i]]); }
      break;
    case 'ContextMenu': e.preventDefault(); openMenu(e, entries[i], i); break;
    default:
  }
});

document.addEventListener('keydown', (e) => {
  if (viewerOpen() || document.querySelector('dialog[open]')) return;
  const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName);
  if (e.key === 'Escape') { if (!menu.hidden) closeMenu(); else clearSelection(); }
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'a' && selectable && !typing) {
    e.preventDefault();
    entries.forEach((en) => selected.add(en.id));
    paintSelection();
  }
});

// ---- drag a row onto a folder to move it ---------------------------------------------------

const DRAG_TYPE = 'application/x-dekave-items';
function wireDrag(row, entry, i) {
  row.draggable = true;
  row.addEventListener('dragstart', (e) => {
    if (!selected.has(entry.id)) { selectOnly(i); paintSelection(); }
    e.dataTransfer.setData(DRAG_TYPE, JSON.stringify([...selected]));
    e.dataTransfer.effectAllowed = 'move';
  });
  if (entry.is_dir) wireDropTarget(row, () => entry.id, () => !selected.has(entry.id));
}
function wireDropTarget(node, target, allowed = () => true) {
  node.addEventListener('dragover', (e) => {
    if (!e.dataTransfer.types.includes(DRAG_TYPE) || !allowed()) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'move';
    node.classList.add('drop-target');
  });
  node.addEventListener('dragleave', () => node.classList.remove('drop-target'));
  node.addEventListener('drop', async (e) => {
    node.classList.remove('drop-target');
    if (!e.dataTransfer.types.includes(DRAG_TYPE)) return;
    e.preventDefault();
    e.stopPropagation();
    const ids = new Set(JSON.parse(e.dataTransfer.getData(DRAG_TYPE)));
    await moveTo(entries.filter((en) => ids.has(en.id)), target());
  });
}

// ---- actions -----------------------------------------------------------------------------------

async function each(items, fn) {
  let failed = 0;
  let last = '';
  for (const item of items) {
    try { await fn(item); } catch (e) { failed++; last = e.message; }
  }
  return { done: items.length - failed, failed, last };
}

function report(result, verb, items) {
  const what = items.length === 1 ? items[0].name : `${result.done} items`;
  if (result.failed === 0) say(`${what} ${verb}`);
  else if (result.done === 0) say(result.last);
  else say(`${result.done} ${verb}, ${result.failed} could not be: ${result.last}`);
}

async function moveTo(items, parent) {
  items = items.filter((e) => (e.parent_id ?? null) !== (parent ?? null));
  if (!items.length) return;
  const result = await each(items, (e) => api(`/api/drive/files/${e.id}/move`, { method: 'POST', body: { parent } }));
  clearSelection();
  await load();
  report(result, 'moved', items);
}

async function act(action, items) {
  if (!items.length) return;
  const one = items[0];
  try {
    switch (action) {
      case 'open': open(one); return;
      case 'download': location.href = `/api/drive/files/${one.id}/content?download=1`; return;
      case 'share': await shareDialog(one, () => { detailsFor = null; renderDetails(); }); return;
      case 'copy': await copyText(shareUrl(one.token)); return;
      case 'revoke':
        await api(`/api/drive/shares/${one.share_id}`, { method: 'DELETE' });
        await load();
        say('Link stopped');
        return;
      case 'rename': {
        const name = await ask('Rename', one.name, 'Rename');
        if (name && name !== one.name) { await api(`/api/drive/files/${one.id}/rename`, { method: 'POST', body: { name } }); await load(); }
        return;
      }
      case 'move': {
        const parent = await pickFolder(items);
        if (parent !== undefined) await moveTo(items, parent);
        return;
      }
      case 'trash': {
        const result = await each(items, (e) => api(`/api/drive/files/${e.id}`, { method: 'DELETE' }));
        clearSelection();
        await load();
        report(result, 'moved to trash', items);
        return;
      }
      case 'restore': {
        const result = await each(items, (e) => api(`/api/drive/trash/${e.id}/restore`, { method: 'POST' }));
        clearSelection();
        await load();
        report(result, 'restored', items);
        return;
      }
      case 'purge': {
        const what = items.length === 1 ? one.name : `${items.length} items`;
        if (!await confirmDanger(`Delete ${what} for good?`, 'This cannot be undone.', 'Delete for good')) return;
        const result = await each(items, (e) => api(`/api/drive/trash/${e.id}`, { method: 'DELETE' }));
        clearSelection();
        await load();
        report(result, 'deleted', items);
        return;
      }
      default:
    }
  } catch (e) {
    say(e.message);
  }
}

// ---- the actions menu ----------------------------------------------------------------------------

function openMenu(event, entry, i) {
  event.preventDefault();
  event.stopPropagation();
  if (selectable && !selected.has(entry.id)) { selectOnly(i); paintSelection(); }
  menuTarget = selectable && selected.size > 1 ? chosen() : [entry];
  const many = menuTarget.length > 1;
  const only = { trash: ['restore', 'purge'], shared: ['copy', 'revoke'] }[view];
  for (const b of menu.querySelectorAll('button')) {
    const a = b.dataset.action;
    b.hidden = only ? !only.includes(a)
      : ['restore', 'purge', 'copy', 'revoke'].includes(a) || (a === 'download' && (entry.is_dir || many)) || (a === 'open' && many) || (many && ['share', 'rename'].includes(a));
    if (a === 'open') b.textContent = entry.is_dir ? 'Open' : previewKind(entry) ? 'Preview' : 'Open';
  }
  menu.hidden = false;
  const at = event.clientX ? { x: event.clientX, y: event.clientY } : (() => { const r = list.children[i].getBoundingClientRect(); return { x: r.right - 200, y: r.bottom }; })();
  menu.style.left = `${Math.max(8, Math.min(at.x, window.innerWidth - menu.offsetWidth - 8))}px`;
  menu.style.top = `${Math.max(8, Math.min(at.y, window.innerHeight - menu.offsetHeight - 8))}px`;
  menu.dataset.row = i;
  menu.querySelector('button:not([hidden])').focus();
}
function closeMenu() {
  if (menu.hidden) return;
  menu.hidden = true;
  const row = list.children[menu.dataset.row];
  if (row) row.focus({ preventScroll: true });
}
document.addEventListener('click', closeMenu);
menu.addEventListener('click', (e) => {
  const action = e.target.closest('button')?.dataset.action;
  const items = menuTarget;
  e.stopPropagation();
  closeMenu();
  if (action && items) act(action, items);
});
menu.addEventListener('keydown', (e) => {
  const items = [...menu.querySelectorAll('button:not([hidden])')];
  const at = items.indexOf(document.activeElement);
  if (e.key === 'ArrowDown') { e.preventDefault(); items[(at + 1) % items.length].focus(); }
  if (e.key === 'ArrowUp') { e.preventDefault(); items[(at - 1 + items.length) % items.length].focus(); }
  if (e.key === 'Tab') { e.preventDefault(); closeMenu(); }
});

// ---- loading ---------------------------------------------------------------------------------------

async function load() {
  let items;
  if (view === 'shared') {
    items = (await api('/api/drive/shares')).map((s) => ({ ...s, share_id: s.id, id: s.file_id * 1e6 + s.id, file_id: s.file_id, size: 0, mime: '' }));
    $('empty-title').textContent = 'Nothing is shared';
    $('empty-text').textContent = 'Use Share… on a file or folder to make a link.';
  } else if (view === 'trash') {
    items = await api('/api/drive/trash');
    $('empty-title').textContent = 'The trash is empty';
    $('empty-text').textContent = 'Deleted items wait here for 30 days, then go for good.';
  } else {
    const id = folderId();
    listing = await api(`/api/drive/files${id ? `?folder=${id}` : ''}`);
    items = listing.entries;
    document.title = listing.folder ? `${listing.folder.name} · DeKave` : 'DeKave';
  }
  entries = sortEntries(items);
  for (const id of [...selected]) if (!entries.some((e) => e.id === id)) selected.delete(id);
  focused = Math.min(focused, entries.length - 1);
  renderCrumbs();
  renderSortHeader();
  renderRows();
  if (view === 'files') for (const a of $('crumbs').querySelectorAll('a')) wireDropTarget(a, () => (a.dataset.folder ? Number(a.dataset.folder) : null));
}

// ---- wiring --------------------------------------------------------------------------------------------

$('nav-files').classList.toggle('current', view === 'files');
$('nav-trash').classList.toggle('current', view === 'trash');
$('nav-shared').classList.toggle('current', view === 'shared');
$('when-label').textContent = view === 'trash' ? 'Deleted' : view === 'shared' ? 'Expires' : 'Changed';
$('select-all').hidden = !selectable;
document.body.dataset.view = view;

for (const b of document.querySelectorAll('.head [data-sort]')) {
  b.addEventListener('click', () => {
    sort = b.dataset.sort === sort.key ? { key: sort.key, dir: -sort.dir } : { key: b.dataset.sort, dir: b.dataset.sort === 'name' ? 1 : -1 };
    localStorage.setItem(SORT_KEY, JSON.stringify(sort));
    const keep = focused >= 0 ? entries[focused]?.id : null;
    entries = sortEntries(entries);
    focused = keep ? entries.findIndex((e) => e.id === keep) : -1;
    renderSortHeader();
    renderRows();
  });
}
$('select-all').addEventListener('click', () => {
  if (selected.size === entries.length) selected.clear(); else entries.forEach((e) => selected.add(e.id));
  paintSelection();
});
$('selection').addEventListener('click', (e) => {
  const action = e.target.closest('[data-action]')?.dataset.action;
  if (action === 'clear') clearSelection();
  else if (action) act(action, chosen());
});
$('details-toggle').addEventListener('click', () => {
  const off = localStorage.getItem('dekave-details') === 'off';
  localStorage.setItem('dekave-details', off ? 'on' : 'off');
  detailsFor = null;
  renderDetails();
});
$('main').addEventListener('click', (e) => { if (e.target === $('main') || e.target === list || e.target.id === 'empty') clearSelection(); });

const fileInput = $('file-input');
$('upload').addEventListener('click', () => fileInput.click());
fileInput.addEventListener('change', () => { uploadAll([...fileInput.files], load); fileInput.value = ''; });
$('new-folder').addEventListener('click', async () => {
  const name = await ask('New folder', '', 'Create');
  if (!name) return;
  try {
    const made = await api('/api/drive/folders', { method: 'POST', body: { parent: folderId(), name } });
    await load();
    const i = entries.findIndex((e) => e.id === made.id);
    if (i >= 0) { selectOnly(i); focusRow(i); paintSelection(); }
  } catch (e) {
    say(e.message);
  }
});
$('empty-trash').addEventListener('click', async () => {
  if (!entries.length) return;
  if (!await confirmDanger('Empty the trash?', `${entries.length} item${entries.length === 1 ? '' : 's'} will be deleted for good.`, 'Empty trash')) return;
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
  // Files dragged in from the desktop. Rows being dragged carry another type and never get here.
  const drop = $('drop');
  const hasFiles = (e) => e.dataTransfer?.types.includes('Files');
  let depth = 0;
  document.addEventListener('dragenter', (e) => { if (!hasFiles(e)) return; e.preventDefault(); depth++; drop.hidden = false; });
  document.addEventListener('dragover', (e) => { if (hasFiles(e)) e.preventDefault(); });
  document.addEventListener('dragleave', (e) => { if (!hasFiles(e)) return; depth = Math.max(0, depth - 1); if (depth === 0) drop.hidden = true; });
  document.addEventListener('drop', (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    drop.hidden = true;
    if (e.dataTransfer.files.length) uploadAll([...e.dataTransfer.files], load);
  });
}

api('/api/me').then((me) => { $('me-name').textContent = me.name; $('nav-people').hidden = !me.is_admin; });
load().catch((e) => { $('empty').hidden = false; $('head').hidden = true; $('empty-title').textContent = e.message; $('empty-text').textContent = ''; });
