// Sign-in and setup forms: post JSON, show the server's sentence on failure, move on when signed in.
const form = document.getElementById('form');
const error = document.getElementById('error');
form.addEventListener('submit', async (event) => {
  event.preventDefault();
  error.textContent = '';
  const button = form.querySelector('button');
  button.disabled = true;
  const body = { name: form.name.value.trim(), password: form.password.value };
  if (form.dataset.token) body.token = form.dataset.token;
  try {
    const response = await fetch(form.dataset.action, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    if (response.ok) {
      const next = new URLSearchParams(location.search).get('next');
      location.replace(next && next.startsWith('/') ? next : form.dataset.next);
      return;
    }
    const data = await response.json().catch(() => ({}));
    error.textContent = data.error || `Failed (${response.status})`;
  } catch (e) {
    error.textContent = 'Could not reach the server.';
  } finally {
    button.disabled = false;
  }
});
