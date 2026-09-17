// The admin's people page: list, add, disable or enable, set a new password.
const list = document.getElementById('list');
const error = document.getElementById('error');
let me = null;

async function api(path, options = {}) {
  if (options.body) {
    options.body = JSON.stringify(options.body);
    options.headers = { 'content-type': 'application/json' };
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

function button(text, className, onclick) {
  return Object.assign(document.createElement('button'), { type: 'button', className, textContent: text, onclick });
}

async function load() {
  const users = await api('/api/users');
  list.replaceChildren();
  for (const u of users) {
    const row = document.createElement('div');
    row.className = `person${u.disabled ? ' off' : ''}`;
    const name = document.createElement('div');
    name.className = 'who';
    name.append(Object.assign(document.createElement('strong'), { textContent: u.name }));
    const tags = [u.is_admin ? 'admin' : null, u.disabled ? 'disabled' : null, u.id === me.id ? 'you' : null].filter(Boolean).join(' · ');
    name.append(Object.assign(document.createElement('small'), { textContent: tags }));
    const actions = document.createElement('div');
    actions.className = 'person-actions';
    actions.append(button('New password', 'ghost', async () => {
      const password = prompt(`New password for ${u.name} (8 characters or more)`);
      if (!password) return;
      try { await api(`/api/users/${u.id}/password`, { method: 'POST', body: { password } }); alert('Password changed.'); } catch (e) { alert(e.message); }
    }));
    if (u.id !== me.id) {
      actions.append(button(u.disabled ? 'Enable' : 'Disable', u.disabled ? 'ghost' : 'ghost danger', async () => {
        try { await api(`/api/users/${u.id}/disabled`, { method: 'POST', body: { disabled: !u.disabled } }); await load(); } catch (e) { alert(e.message); }
      }));
    }
    row.append(name, actions);
    list.append(row);
  }
}

document.getElementById('add').addEventListener('submit', async (event) => {
  event.preventDefault();
  error.textContent = '';
  const form = event.target;
  try {
    await api('/api/users', { method: 'POST', body: { name: form.name.value.trim(), password: form.password.value, is_admin: document.getElementById('is-admin').checked } });
    form.reset();
    await load();
  } catch (e) {
    error.textContent = e.message;
  }
});

try {
  me = await api('/api/me');
  await load();
} catch (e) {
  list.replaceChildren(Object.assign(document.createElement('p'), { className: 'denied', textContent: e.message === 'not allowed' ? 'Only an admin can manage people.' : e.message }));
  document.getElementById('add').hidden = true;
}
