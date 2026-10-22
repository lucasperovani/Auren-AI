// Auren-AI web frontend (spec section 5).
// Vanilla ES module SPA served by the same application server as the API.

const API = '/api/v1';
let sessionToken = sessionStorage.getItem('auren_token') || '';
let currentUser = null;
let ws = null;
let wsReconnectDelay = 1000;

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async function api(method, path, body, isForm) {
  const headers = {};
  if (sessionToken) headers['Authorization'] = `Bearer ${sessionToken}`;
  if (body && !isForm) headers['Content-Type'] = 'application/json';
  const response = await fetch(`${API}${path}`, {
    method,
    headers,
    body: body ? (isForm ? body : JSON.stringify(body)) : undefined,
  });
  if (response.status === 401 && sessionToken) {
    // Session expired: back to the auth gate.
    setToken('');
    showAuth();
    throw new Error('Session expired');
  }
  if (!response.ok) {
    let message = `HTTP ${response.status}`;
    try {
      const payload = await response.json();
      if (payload.error) message = payload.error.message || payload.error.code;
    } catch { /* non-JSON error body */ }
    throw new Error(message);
  }
  if (response.status === 204) return null;
  return response.json();
}

function setToken(token) {
  sessionToken = token;
  if (token) sessionStorage.setItem('auren_token', token);
  else sessionStorage.removeItem('auren_token');
}

function toast(message, kind = '') {
  const el = document.createElement('div');
  el.className = `toast ${kind}`;
  el.textContent = message;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), 5000);
}

function esc(text) {
  const div = document.createElement('div');
  div.textContent = text ?? '';
  return div.innerHTML;
}

// ---------------------------------------------------------------------------
// Auth gate
// ---------------------------------------------------------------------------

async function showAuth() {
  document.getElementById('main-view').classList.add('hidden');
  document.getElementById('auth-view').classList.remove('hidden');
  const { setup_required: setupRequired } = await api('GET', '/auth/setup-status');
  const subtitle = document.getElementById('auth-subtitle');
  const displayName = document.getElementById('auth-display-name');
  const submit = document.getElementById('auth-submit');
  if (setupRequired) {
    subtitle.textContent = 'Create the administrator account to get started.';
    displayName.classList.remove('hidden');
    submit.textContent = 'Create account';
  } else {
    subtitle.textContent = 'Sign in to your workspace.';
    displayName.classList.add('hidden');
    submit.textContent = 'Sign in';
  }
  document.getElementById('auth-form').dataset.mode = setupRequired ? 'setup' : 'login';
}

document.getElementById('auth-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const errorEl = document.getElementById('auth-error');
  errorEl.classList.add('hidden');
  const form = event.target;
  const email = document.getElementById('auth-email').value.trim();
  const password = document.getElementById('auth-password').value;
  try {
    if (form.dataset.mode === 'setup') {
      const displayName = document.getElementById('auth-display-name').value.trim();
      await api('POST', '/auth/setup', { email, display_name: displayName, password });
    }
    const { session_token: token } = await api('POST', '/auth/login', { email, password });
    setToken(token);
    await enterApp();
  } catch (err) {
    errorEl.textContent = err.message;
    errorEl.classList.remove('hidden');
  }
});

document.getElementById('logout-btn').addEventListener('click', async () => {
  try { await api('POST', '/auth/logout', { session_token: sessionToken }); } catch { /* already invalid */ }
  setToken('');
  if (ws) { ws.close(); ws = null; }
  showAuth();
});

// ---------------------------------------------------------------------------
// App shell
// ---------------------------------------------------------------------------

async function enterApp() {
  document.getElementById('auth-view').classList.add('hidden');
  document.getElementById('main-view').classList.remove('hidden');
  currentUser = await api('POST', '/auth/me', { session_token: sessionToken });
  document.getElementById('user-name').textContent = currentUser.display_name;
  connectWebSocket();
  navigate(location.hash.replace('#', '') || 'dashboard');
}

const views = {};
window.addEventListener('hashchange', () => navigate(location.hash.replace('#', '') || 'dashboard'));

document.querySelectorAll('.nav-link').forEach((link) => {
  link.addEventListener('click', () => {
    document.getElementById('sidebar').classList.remove('open');
  });
});
document.getElementById('menu-toggle').addEventListener('click', () => {
  document.getElementById('sidebar').classList.toggle('open');
});

function navigate(view) {
  const content = document.getElementById('content');
  document.querySelectorAll('.nav-link').forEach((l) => l.classList.toggle('active', l.dataset.view === view));
  const render = views[view] || views.dashboard;
  render(content).catch((err) => {
    content.innerHTML = `<div class="empty-state">Failed to load: ${esc(err.message)}</div>`;
  });
}

// ---------------------------------------------------------------------------
// WebSocket live events (docs/api/websocket.md)
// ---------------------------------------------------------------------------

async function connectWebSocket() {
  if (ws) ws.close();
  try {
    const { ticket } = await api('POST', '/events/ticket', { session_token: sessionToken });
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${location.host}/api/v1/events?ticket=${encodeURIComponent(ticket)}`);
    ws.onopen = () => {
      wsReconnectDelay = 1000;
      document.getElementById('connection-status').className = 'conn-status live';
    };
    ws.onmessage = (event) => {
      try {
        const envelope = JSON.parse(event.data);
        onLiveEvent(envelope);
      } catch { /* ignore malformed frames */ }
    };
    ws.onclose = () => {
      document.getElementById('connection-status').className = 'conn-status off';
      // Reconnect with exponential backoff; state is re-fetched from REST.
      setTimeout(connectWebSocket, wsReconnectDelay);
      wsReconnectDelay = Math.min(wsReconnectDelay * 2, 30000);
    };
  } catch {
    document.getElementById('connection-status').className = 'conn-status off';
    setTimeout(connectWebSocket, wsReconnectDelay);
    wsReconnectDelay = Math.min(wsReconnectDelay * 2, 30000);
  }
}

function onLiveEvent(envelope) {
  if (envelope.event_type === 'notification.created') {
    toast('New notification', 'success');
  } else if (envelope.event_type === 'task_run.approval_required') {
    toast('An operation requires your approval', 'error');
  } else if (envelope.event_type.startsWith('task.')) {
    // Refresh the current view when task state changes.
    navigate(location.hash.replace('#', '') || 'dashboard');
  }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

views.dashboard = async (content) => {
  const [tasks, skills, integrations, approvals, notifications] = await Promise.all([
    api('GET', '/tasks?limit=5'),
    api('GET', '/skills'),
    api('GET', '/integrations'),
    api('GET', '/approvals?status=pending'),
    api('GET', '/notifications?limit=5'),
  ]);
  const activeTasks = tasks.items.filter((t) => t.status === 'active' || t.status === 'draft').length;
  content.innerHTML = `
    <h2 class="section-title">Dashboard</h2>
    <div class="grid">
      <div class="card"><div class="stat">${activeTasks}</div><div class="stat-label">Active tasks</div></div>
      <div class="card"><div class="stat">${approvals.items.length}</div><div class="stat-label">Pending approvals</div></div>
      <div class="card"><div class="stat">${skills.items.length}</div><div class="stat-label">Installed skills</div></div>
      <div class="card"><div class="stat">${integrations.items.length}</div><div class="stat-label">Integrations</div></div>
    </div>
    <h3 class="section-title">Recent tasks</h3>
    ${tasks.items.length ? `<table class="table">
      <thead><tr><th>Task</th><th>Status</th><th>Updated</th></tr></thead>
      <tbody>${tasks.items.map((t) => `
        <tr><td><a href="#tasks">${esc(t.title)}</a></td>
        <td><span class="badge ${esc(t.status)}">${esc(t.status)}</span></td>
        <td class="muted">${esc(t.updated_at)}</td></tr>`).join('')}
      </tbody></table>` : '<div class="empty-state">No tasks yet. Create one from the Chat view.</div>'}
    <h3 class="section-title">Recent notifications</h3>
    ${notifications.items.length ? notifications.items.map((n) => `
      <div class="card" style="margin-bottom:8px"><strong>${esc(n.title)}</strong>
      <div class="muted">${esc(n.body)}</div></div>`).join('') : '<div class="empty-state">No notifications.</div>'}
  `;
};

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

views.chat = async (content) => {
  const agents = await api('GET', '/agents');
  content.innerHTML = `
    <div class="chat-container">
      <div class="form-row" style="max-width:280px">
        <select id="chat-agent">
          ${agents.items.map((a) => `<option value="${esc(a.id)}">${esc(a.name)}</option>`).join('')}
        </select>
      </div>
      <div class="chat-messages" id="chat-messages"></div>
      <div class="chat-input-row">
        <input id="chat-input" placeholder="Ask anything, or request a recurring task…" autocomplete="off">
        <button class="btn" id="chat-send">Send</button>
      </div>
    </div>`;
  const input = document.getElementById('chat-input');
  const send = document.getElementById('chat-send');
  const submit = async () => {
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    addBubble('user', text);
    const agentId = document.getElementById('chat-agent').value;
    try {
      // Create (or reuse) a conversation with the selected agent.
      const conversation = await api('POST', '/conversations', { agent_id: agentId, title: text.slice(0, 60) });
      await api('POST', `/conversations/${conversation.id}/messages`, { content: text });
      addBubble('assistant', 'Request received and queued for processing. Track progress in Tasks.');
    } catch (err) {
      addBubble('assistant', `Error: ${err.message}`);
    }
  };
  send.addEventListener('click', submit);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') submit(); });
};

function addBubble(role, text) {
  const container = document.getElementById('chat-messages');
  const bubble = document.createElement('div');
  bubble.className = `chat-bubble ${role}`;
  bubble.textContent = text;
  container.appendChild(bubble);
  container.scrollTop = container.scrollHeight;
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

views.tasks = async (content) => {
  const { items } = await api('GET', '/tasks?limit=100');
  content.innerHTML = `
    <h2 class="section-title">Tasks</h2>
    ${items.length ? `<table class="table">
      <thead><tr><th>Name</th><th>Status</th><th>Schedule</th><th>Actions</th></tr></thead>
      <tbody>${items.map((t) => `
        <tr data-id="${esc(t.id)}">
          <td><strong>${esc(t.title)}</strong><div class="muted">${esc(t.objective)}</div></td>
          <td><span class="badge ${esc(t.status)}">${esc(t.status)}</span></td>
          <td class="muted">${t.schedule ? esc(t.schedule.cron || '') : '—'}</td>
          <td>
            <button class="btn btn-sm btn-secondary" data-action="run">Run</button>
            <button class="btn btn-sm btn-secondary" data-action="pause">Pause</button>
            <button class="btn btn-sm btn-secondary" data-action="resume">Resume</button>
            <button class="btn btn-sm btn-danger" data-action="cancel">Cancel</button>
          </td>
        </tr>`).join('')}
      </tbody></table>` : '<div class="empty-state">No tasks. Ask the agent in Chat to create one.</div>'}
  `;
  content.querySelectorAll('tr[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const taskId = btn.closest('tr').dataset.id;
      const action = btn.dataset.action;
      try {
        if (action === 'run') await api('POST', `/tasks/${taskId}/run`, {});
        else await api('POST', `/tasks/${taskId}/${action}`, {});
        toast(`Task ${action} accepted`, 'success');
        navigate('tasks');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

views.skills = async (content) => {
  const { items } = await api('GET', '/skills');
  content.innerHTML = `
    <h2 class="section-title">Skills</h2>
    <div class="card" style="margin-bottom:16px">
      <h3>Install a skill package</h3>
      <p class="muted" style="margin-bottom:10px">Upload a skill ZIP. It is validated before installation.</p>
      <input type="file" id="skill-file" accept=".zip">
      <button class="btn" id="skill-upload" style="margin-top:10px">Upload and install</button>
    </div>
    ${items.length ? `<div class="grid">${items.map((s) => `
      <div class="card" data-id="${esc(s.id)}">
        <h3>${esc(s.name)}</h3>
        <p class="muted" style="margin-bottom:8px">${esc(s.id)}</p>
        <span class="badge ${esc(s.status)}">${esc(s.status)}</span>
        <div style="margin-top:12px">
          <button class="btn btn-sm btn-secondary" data-action="enable">Enable</button>
          <button class="btn btn-sm btn-secondary" data-action="disable">Disable</button>
          <button class="btn btn-sm btn-danger" data-action="uninstall">Uninstall</button>
        </div>
      </div>`).join('')}</div>` : '<div class="empty-state">No skills installed yet.</div>'}
  `;
  document.getElementById('skill-upload').addEventListener('click', async () => {
    const fileInput = document.getElementById('skill-file');
    if (!fileInput.files.length) { toast('Select a ZIP file first', 'error'); return; }
    const form = new FormData();
    form.append('file', fileInput.files[0]);
    try {
      const { upload_id: uploadId } = await api('POST', '/skills/uploads', form, true);
      toast('Upload accepted; validating…', 'success');
      // Poll the status endpoint until the pipeline finishes.
      const poll = setInterval(async () => {
        const status = await api('GET', `/skills/uploads/${uploadId}`);
        if (['installed', 'failed', 'cancelled'].includes(status.status)) {
          clearInterval(poll);
          if (status.status === 'installed') toast(`Skill ${status.skill_id} installed`, 'success');
          else toast(`Installation failed: ${status.error_message || 'unknown error'}`, 'error');
          navigate('skills');
        }
      }, 1000);
    } catch (err) { toast(err.message, 'error'); }
  });
  content.querySelectorAll('.card[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const skillId = btn.closest('.card').dataset.id;
      try {
        await api('POST', `/skills/${skillId}/${btn.dataset.action}`, {});
        toast(`Skill ${btn.dataset.action} done`, 'success');
        navigate('skills');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

views.integrations = async (content) => {
  const [catalog, connections] = await Promise.all([
    api('GET', '/integrations/catalog'),
    api('GET', '/integrations'),
  ]);
  content.innerHTML = `
    <h2 class="section-title">Integrations</h2>
    <div class="card" style="margin-bottom:16px">
      <h3>Connect a provider</h3>
      <div class="form-row" style="margin-top:10px; max-width:320px">
        <select id="integration-type">
          ${catalog.items.map((c) => `<option value="${esc(c.provider_type)}">${esc(c.provider_type)}</option>`).join('')}
        </select>
        <input id="integration-name" placeholder="Display name">
        <button class="btn" id="integration-connect">Connect</button>
      </div>
    </div>
    ${connections.items.length ? `<table class="table">
      <thead><tr><th>Provider</th><th>Name</th><th>Status</th><th></th></tr></thead>
      <tbody>${connections.items.map((i) => `
        <tr data-id="${esc(i.id)}">
          <td>${esc(i.provider_type)}</td><td>${esc(i.display_name)}</td>
          <td><span class="badge ${esc(i.status)}">${esc(i.status)}</span></td>
          <td><button class="btn btn-sm btn-danger" data-action="disconnect">Disconnect</button></td>
        </tr>`).join('')}
      </tbody></table>` : '<div class="empty-state">No integrations connected.</div>'}
  `;
  document.getElementById('integration-connect').addEventListener('click', async () => {
    const providerType = document.getElementById('integration-type').value;
    const displayName = document.getElementById('integration-name').value.trim();
    if (!displayName) { toast('Enter a display name', 'error'); return; }
    try {
      await api('POST', `/integrations/${providerType}/connect`, { display_name: displayName });
      toast('Integration connected', 'success');
      navigate('integrations');
    } catch (err) { toast(err.message, 'error'); }
  });
  content.querySelectorAll('tr[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await api('DELETE', `/integrations/${btn.closest('tr').dataset.id}`);
        toast('Disconnected', 'success');
        navigate('integrations');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

// ---------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------

views.approvals = async (content) => {
  const { items } = await api('GET', '/approvals?status=pending');
  content.innerHTML = `
    <h2 class="section-title">Approval inbox</h2>
    ${items.length ? items.map((a) => `
      <div class="card" style="margin-bottom:12px" data-id="${esc(a.id)}">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:8px">
          <span class="badge ${esc(a.status)}">${esc(a.status)}</span>
          <span class="muted">expires ${esc(a.expires_at)}</span>
        </div>
        <div class="form-row" style="flex-direction:row; gap:8px">
          <button class="btn" data-action="approve">Approve</button>
          <button class="btn btn-danger" data-action="reject">Reject</button>
        </div>
      </div>`).join('') : '<div class="empty-state">No pending approvals.</div>'}
  `;
  content.querySelectorAll('.card[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const approvalId = btn.closest('.card').dataset.id;
      try {
        // The exact operation is fetched and echoed back so the server can
        // verify the hash (spec section 3.11).
        const detail = await api('GET', `/approvals/${approvalId}`);
        await api('POST', `/approvals/${approvalId}/${btn.dataset.action}`, { operation: detail.operation });
        toast(`Approval ${btn.dataset.action}d`, 'success');
        navigate('approvals');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

views.settings = async (content) => {
  const [providers, workspaceConfig] = await Promise.all([
    api('GET', '/llm/providers'),
    api('GET', '/workspace/config'),
  ]);
  content.innerHTML = `
    <h2 class="section-title">LLM providers</h2>
    <div class="card" style="margin-bottom:16px">
      <div class="form-row" style="max-width:420px">
        <input id="provider-name" placeholder="Name">
        <select id="provider-type">
          <option value="openai_compatible">OpenAI-compatible</option>
          <option value="ollama">Ollama</option>
        </select>
        <input id="provider-url" placeholder="Base URL (https://…)">
        <input id="provider-key" type="password" placeholder="API key (optional)">
        <button class="btn" id="provider-add">Add provider</button>
      </div>
    </div>
    ${providers.items.length ? providers.items.map((p) => `
      <div class="card" style="margin-bottom:8px">
        <strong>${esc(p.name)}</strong> <span class="muted">${esc(p.provider_type)} · ${esc(p.base_url)}</span>
        <span class="badge ${esc(p.status)}">${esc(p.status)}</span>
      </div>`).join('') : '<p class="muted">No providers configured. The app works without one, but the agent needs an LLM to plan tasks.</p>'}
    <h2 class="section-title">Workspace</h2>
    <div class="card"><pre style="font-size:.85rem">${esc(JSON.stringify(workspaceConfig, null, 2))}</pre></div>
  `;
  document.getElementById('provider-add').addEventListener('click', async () => {
    try {
      await api('POST', '/llm/providers', {
        name: document.getElementById('provider-name').value.trim(),
        provider_type: document.getElementById('provider-type').value,
        base_url: document.getElementById('provider-url').value.trim(),
        api_key: document.getElementById('provider-key').value || null,
      });
      toast('Provider added', 'success');
      navigate('settings');
    } catch (err) { toast(err.message, 'error'); }
  });
};

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

(async function boot() {
  if (!sessionToken) { showAuth(); return; }
  try {
    await enterApp();
  } catch {
    setToken('');
    showAuth();
  }
})();