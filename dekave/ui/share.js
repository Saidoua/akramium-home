// The Share… dialog: the item's links, with copy and stop, and a new link with an expiry.

import { $, api, copyText, el, expiryText, say } from '/drive/lib.js';

export const shareUrl = (token) => `${location.origin}/s/${token}`;

export async function shareDialog(entry, changed = () => {}) {
  const dialog = $('share');
  const list = $('share-links');
  $('share-title').textContent = `Share ${entry.name}`;

  async function refresh() {
    const shares = await api(`/api/drive/files/${entry.id}/shares`);
    list.replaceChildren();
    if (!shares.length) list.append(el('p', { className: 'dialog-note', textContent: 'No links yet.' }));
    for (const s of shares) {
      const input = el('input', { readOnly: true, value: shareUrl(s.token), ariaLabel: 'Link' });
      input.onfocus = () => input.select();
      const copy = el('button', { type: 'button', className: 'ghost', textContent: 'Copy' });
      copy.onclick = () => copyText(input.value, input);
      const stop = el('button', { type: 'button', className: 'ghost danger', textContent: 'Stop' });
      stop.onclick = async () => { await api(`/api/drive/shares/${s.id}`, { method: 'DELETE' }); await refresh(); changed(); };
      list.append(el('div', { className: 'share-link' }, input, copy, stop, el('small', { textContent: expiryText(s.expires_at) })));
    }
  }

  $('share-create').onclick = async () => {
    const value = $('share-expiry').value;
    try {
      await api(`/api/drive/files/${entry.id}/shares`, { method: 'POST', body: { expires_in: value ? Number(value) : null } });
      await refresh();
      changed();
      const first = list.querySelector('input');
      if (first) first.focus();
    } catch (e) {
      say(e.message);
    }
  };
  $('share-close').onclick = () => dialog.close();
  await refresh();
  dialog.showModal();
}
