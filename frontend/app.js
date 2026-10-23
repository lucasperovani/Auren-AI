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
  // Sidebar collapse toggle (persisted).
  const sidebar = document.getElementById('sidebar');
  const collapseBtn = document.getElementById('sidebar-collapse');
  const applyCollapse = (collapsed) => {
    sidebar.classList.toggle('collapsed', collapsed);
    document.body.classList.toggle('sidebar-collapsed', collapsed);
    collapseBtn.textContent = collapsed ? '▶' : '◀';
    sessionStorage.setItem('auren_sidebar_collapsed', collapsed ? '1' : '');
  };
  collapseBtn.onclick = () => applyCollapse(!sidebar.classList.contains('collapsed'));
  applyCollapse(sessionStorage.getItem('auren_sidebar_collapsed') === '1');
  refreshApprovalsBadge();
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
    refreshApprovalsBadge();
  } else if (envelope.event_type === 'approval.updated') {
    refreshApprovalsBadge();
  } else if (envelope.event_type.startsWith('task.')) {
    // Refresh the current view when task state changes.
    navigate(location.hash.replace('#', '') || 'dashboard');
  }
}

/// Update the red pending-approvals badge next to the sidebar link.
async function refreshApprovalsBadge() {
  const badge = document.getElementById('approvals-badge');
  if (!badge) return;
  try {
    const { items } = await api('GET', '/approvals?status=pending&limit=100');
    const count = items.length;
    badge.textContent = count > 99 ? '99+' : String(count);
    badge.classList.toggle('hidden', count === 0);
  } catch { /* badge is best-effort */ }
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

// Current conversation state survives navigation between views.
let currentConversationId = sessionStorage.getItem('auren_conv') || '';
let chatLoadedForAgent = '';

views.chat = async (content) => {
  const [conversations, providers] = await Promise.all([
    api('GET', '/conversations?limit=100'),
    api('GET', '/llm/providers'),
  ]);

  // Ensure a default agent exists (system-managed; the user never creates
  // agents manually).
  let agents = (await api('GET', '/agents')).items;
  if (!agents.length) {
    await api('POST', '/agents', { name: 'Assistant', instructions: '' });
    agents = (await api('GET', '/agents')).items;
  }
  const defaultAgent = agents[0];

  // Restore the last-open conversation, or the most recent one.
  if (!currentConversationId || !conversations.items.some((c) => c.id === currentConversationId)) {
    currentConversationId = conversations.items[0]?.id || '';
  }
  if (currentConversationId) sessionStorage.setItem('auren_conv', currentConversationId);

  content.innerHTML = `
    <div class="chat-layout">
      <aside class="chat-sidebar" id="chat-sidebar">
        <button class="btn" id="chat-new" style="width:100%">+ New chat</button>
        <div class="chat-list" id="chat-list">
          ${conversations.items.map((c) => `
            <div class="chat-list-item ${c.id === currentConversationId ? 'selected' : ''}" data-conv="${esc(c.id)}">
              <div class="chat-list-row">
                <div class="chat-list-text">
                  <div class="chat-list-title">${esc(c.title || 'New chat')}</div>
                  <div class="chat-list-meta muted">${esc((c.updated_at || '').slice(0, 16).replace('T', ' '))}</div>
                </div>
                <button class="chat-delete" data-delete="${esc(c.id)}" title="Delete chat">✕</button>
              </div>
            </div>`).join('') || '<div class="muted" style="padding:8px; font-size:.85rem">No chats yet</div>'}
        </div>
      </aside>
      <div class="chat-main">
        <div class="chat-toolbar">
          <span class="muted" style="font-size:.85rem">Agent: <strong>${esc(defaultAgent.name)}</strong></span>
          <span class="muted" style="font-size:.85rem">Model:</span>
          <select id="chat-model">
            <option value="">Default</option>
            ${providers.items.flatMap((p) => (chatModels[p.id] || []).map((m) => `<option value="${esc(m.id)}">${esc(p.name)} / ${esc(m.model_name)}</option>`)).join('')}
          </select>
          <button class="btn btn-sm btn-secondary" id="chat-model-save" style="display:none">Save</button>
          <span class="spacer"></span>
          <label class="muted" style="font-size:.85rem; display:flex; align-items:center; gap:6px; cursor:pointer" title="Run tool executions in this chat without asking for approval">
            <input type="checkbox" id="chat-auto-approve"> Auto-approve
          </label>
        </div>
        <div class="chat-messages" id="chat-messages"></div>
        <div class="chat-input-row">
          <input id="chat-input" placeholder="Ask anything…" autocomplete="off">
          <button class="btn" id="chat-send">Send</button>
        </div>
      </div>
    </div>`;

  // Load models for the model picker.
  const modelSelect = document.getElementById('chat-model');
  const saveBtn = document.getElementById('chat-model-save');
  chatModels = {};
  await Promise.all(providers.items.map(async (p) => {
    try {
      const { items } = await api('GET', `/llm/providers/${p.id}/models`);
      chatModels[p.id] = items;
    } catch { chatModels[p.id] = []; }
  }));
  modelSelect.innerHTML = '<option value="">Default</option>' +
    providers.items.flatMap((p) => (chatModels[p.id] || []).map((m) => `<option value="${esc(m.id)}">${esc(p.name)} / ${esc(m.model_name)}</option>`)).join('');
  // Reflect the agent's current model.
  const agentDetail = await api('GET', `/agents/${defaultAgent.id}`);
  modelSelect.value = agentDetail.model_id || '';
  modelSelect.addEventListener('change', () => { saveBtn.style.display = ''; });
  saveBtn.addEventListener('click', async () => {
    try {
      await api('PATCH', `/agents/${defaultAgent.id}`, { model_id: modelSelect.value || null });
      toast('Model saved', 'success');
      saveBtn.style.display = 'none';
    } catch (err) { toast(err.message, 'error'); }
  });

  // Per-chat auto-approve override (null = follow the global setting).
  const chatAutoApprove = document.getElementById('chat-auto-approve');
  if (currentConversationId) {
    try {
      const { enabled } = await api('GET', `/conversations/${currentConversationId}/auto-approve`);
      chatAutoApprove.checked = enabled === true;
      chatAutoApprove.indeterminate = enabled === null || enabled === undefined;
    } catch { /* default to global */ }
  } else {
    chatAutoApprove.indeterminate = true;
  }
  chatAutoApprove.addEventListener('change', async () => {
    if (!currentConversationId) { toast('Send a message first to start the chat', 'error'); chatAutoApprove.checked = false; return; }
    try {
      await api('PUT', `/conversations/${currentConversationId}/auto-approve`, { enabled: chatAutoApprove.checked });
      toast(chatAutoApprove.checked ? 'Auto-approve on for this chat' : 'Auto-approve off for this chat', 'success');
    } catch (err) {
      toast(err.message, 'error');
      chatAutoApprove.checked = !chatAutoApprove.checked;
    }
  });

  // Conversation switching.
  content.querySelectorAll('.chat-list-item').forEach((item) => {
    item.addEventListener('click', (e) => {
      if (e.target.closest('.chat-delete')) return; // delete button handles it
      currentConversationId = item.dataset.conv;
      sessionStorage.setItem('auren_conv', currentConversationId);
      navigate('chat');
    });
  });
  // Delete chats.
  content.querySelectorAll('.chat-delete').forEach((btn) => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const convId = btn.dataset.delete;
      if (!confirm('Delete this chat?')) return;
      try {
        await api('DELETE', `/conversations/${convId}`);
        if (currentConversationId === convId) {
          currentConversationId = '';
          sessionStorage.removeItem('auren_conv');
        }
        toast('Chat deleted', 'success');
        navigate('chat');
      } catch (err) { toast(err.message, 'error'); }
    });
  });

  // New chat: create a conversation lazily on first message; clear the view.
  document.getElementById('chat-new').addEventListener('click', () => {
    currentConversationId = '';
    sessionStorage.removeItem('auren_conv');
    document.getElementById('chat-messages').innerHTML = '';
    content.querySelectorAll('.chat-list-item').forEach((i) => i.classList.remove('selected'));
  });

  const messagesEl = document.getElementById('chat-messages');
  const input = document.getElementById('chat-input');
  const send = document.getElementById('chat-send');

  // Load the conversation history.
  const loadHistory = async () => {
    if (!currentConversationId) return;
    try {
      const { items } = await api('GET', `/conversations/${currentConversationId}/messages?limit=100`);
      for (const m of items) {
        const bubble = addBubble(m.role === 'assistant' ? 'assistant' : 'user', m.content);
        if (m.tool_trace) addToolTrace(bubble, m.tool_trace);
      }
    } catch { /* conversation not readable */ }
  };
  await loadHistory();
  messagesEl.scrollTop = messagesEl.scrollHeight;

  const submit = async () => {
    const text = input.value.trim();
    if (!text) return;
    input.value = '';
    addBubble('user', text);
    const pending = addBubble('assistant', '…');
    try {
      // Create the conversation lazily on the first message of a new chat.
      if (!currentConversationId) {
        const conversation = await api('POST', '/conversations', { agent_id: defaultAgent.id, title: text.slice(0, 60) });
        currentConversationId = conversation.id;
        sessionStorage.setItem('auren_conv', currentConversationId);
      }
      const result = await api('POST', `/conversations/${currentConversationId}/messages`, { content: text });
      pending.remove();
      const replyBubble = addBubble('assistant', result.reply);
      if (result.error) replyBubble.classList.add('error-bubble');
      addToolTrace(replyBubble, result.tool_trace);
      // Update the sidebar in place: move/insert this conversation at the top
      // without re-rendering the chat (which would lose the transcript).
      updateChatSidebar(text.slice(0, 60));
    } catch (err) {
      pending.textContent = `Error: ${err.message}`;
      pending.classList.add('error-bubble');
    }
  };
  send.addEventListener('click', submit);
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') submit(); });
};

// Model cache for the chat model picker, refreshed per render.
let chatModels = {};
views.tasks = async (content) => {
  const [agents, { items }] = await Promise.all([
    api('GET', '/agents'),
    api('GET', '/tasks?limit=100'),
  ]);

  content.innerHTML = `
    <div style="display:flex; align-items:center; gap:12px">
      <h2 class="section-title" style="margin:0">Tasks</h2>
      <button class="btn" id="task-new">+ New task</button>
    </div>
    <div id="task-form-container"></div>
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
            <button class="btn btn-sm btn-secondary" data-action="edit">Edit</button>
            <button class="btn btn-sm btn-secondary" data-action="history">History</button>
            <button class="btn btn-sm btn-danger" data-action="delete">Delete</button>
            <button class="btn btn-sm btn-danger" data-action="cancel">Cancel</button>
          </td>
        </tr>`).join('')}
      </tbody></table>` : '<div class="empty-state">No tasks yet. Create one manually or ask the agent in Chat.</div>'}
  `;

  // Inline create/edit form.
  const formContainer = document.getElementById('task-form-container');
  const showForm = (task) => {
    const isEdit = Boolean(task);
    formContainer.innerHTML = `
      <div class="card" style="margin:16px 0; max-width:560px">
        <h3>${isEdit ? 'Edit task' : 'New task'}</h3>
        <div class="form-row" style="margin-top:12px">
          <input id="task-title" placeholder="Title" value="${esc(task?.title || '')}">
          <textarea id="task-objective" placeholder="Objective (what should the agent do?)">${esc(task?.objective || '')}</textarea>
          <select id="task-agent">${agents.items.map((a) => `<option value="${esc(a.id)}" ${task?.agent_id === a.id ? 'selected' : ''}>${esc(a.name)}</option>`).join('')}</select>
          <input id="task-cron" placeholder="Schedule (cron, empty = manual only)" value="${esc(task?.schedule?.cron || '')}">
          <div style="display:flex; gap:8px">
            <button class="btn" id="task-save">${isEdit ? 'Save changes' : 'Create task'}</button>
            <button class="btn btn-secondary" id="task-cancel-form">Cancel</button>
          </div>
        </div>
      </div>`;
    document.getElementById('task-cancel-form').addEventListener('click', () => { formContainer.innerHTML = ''; });
    document.getElementById('task-save').addEventListener('click', async () => {
      const title = document.getElementById('task-title').value.trim();
      const objective = document.getElementById('task-objective').value.trim();
      const agentId = document.getElementById('task-agent').value;
      const cron = document.getElementById('task-cron').value.trim();
      if (!title || !objective) { toast('Title and objective are required', 'error'); return; }
      if (!agentId) { toast('Create an agent first (Chat view)', 'error'); return; }
      const body = { title, objective, agent_id: agentId };
      if (cron) body.schedule = { cron };
      try {
        if (isEdit) {
          await api('PATCH', `/tasks/${task.id}`, body);
          toast('Task updated', 'success');
        } else {
          await api('POST', '/tasks', body);
          toast('Task created', 'success');
        }
        navigate('tasks');
      } catch (err) { toast(err.message, 'error'); }
    });
  };
  document.getElementById('task-new').addEventListener('click', () => showForm(null));

  content.querySelectorAll('tr[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const taskId = btn.closest('tr').dataset.id;
      const action = btn.dataset.action;
      try {
        if (action === 'edit') {
          const task = items.find((t) => t.id === taskId);
          showForm(task);
          return;
        }
        if (action === 'history') {
          showRunHistory(taskId, items.find((t) => t.id === taskId));
          return;
        }
        if (action === 'delete') {
          if (!confirm('Delete this task?')) return;
          await api('DELETE', `/tasks/${taskId}`);
          toast('Task deleted', 'success');
        } else if (action === 'run') {
          await api('POST', `/tasks/${taskId}/run`, {});
          toast('Run accepted', 'success');
        } else {
          await api('POST', `/tasks/${taskId}/${action}`, {});
          toast(`Task ${action} accepted`, 'success');
        }
        navigate('tasks');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

/// Show a task's execution history: when each run happened, its status, the
/// result, and the recorded steps (tool calls with parameters and outputs).
async function showRunHistory(taskId, task) {
  const container = document.getElementById('task-form-container');
  container.innerHTML = '<div class="card" style="margin:16px 0"><p class="muted">Loading history…</p></div>';
  try {
    const { items: runs } = await api('GET', `/tasks/${taskId}/runs?limit=20`);
    const rows = await Promise.all(runs.map(async (run) => {
      let stepsHtml = '<div class="muted" style="font-size:.85rem">No recorded steps</div>';
      try {
        const { items: steps } = await api('GET', `/task-runs/${run.id}/steps?limit=50`);
        if (steps.length) {
          stepsHtml = steps.map((s) => `
            <details class="run-step">
              <summary>Step ${s.step_index} — <span class="badge ${esc(s.status)}">${esc(s.status)}</span></summary>
              <div class="run-step-body">
                <div><strong>Input:</strong></div>
                <pre>${esc(JSON.stringify(s.input, null, 2))}</pre>
                ${s.output ? `<div><strong>Output:</strong></div><pre>${esc(JSON.stringify(s.output, null, 2))}</pre>` : ''}
                ${s.error_message ? `<div class="error">${esc(s.error_message)}</div>` : ''}
              </div>
            </details>`).join('');
        }
      } catch { /* steps unavailable */ }
      return `
        <details class="run-entry">
          <summary>
            <span class="badge ${esc(run.status)}">${esc(run.status)}</span>
            <span class="muted" style="font-size:.85rem">
              ${esc(run.trigger)} · started ${esc((run.started_at || '—').slice(0, 19).replace('T', ' '))}
              ${run.finished_at ? `· finished ${esc(run.finished_at.slice(0, 19).replace('T', ' '))}` : ''}
            </span>
          </summary>
          <div class="run-step-body">
            ${run.result ? `<div><strong>Result:</strong></div><pre>${esc(JSON.stringify(run.result, null, 2))}</pre>` : ''}
            ${run.error_message ? `<div class="error">${esc(run.error_message)}</div>` : ''}
            <div style="margin-top:8px"><strong>Steps:</strong></div>
            ${stepsHtml}
          </div>
        </details>`;
    }));
    container.innerHTML = `
      <div class="card" style="margin:16px 0">
        <div style="display:flex; align-items:center; gap:10px">
          <h3>Execution history — ${esc(task?.title || '')}</h3>
          <span class="spacer"></span>
          <button class="btn btn-sm btn-secondary" id="history-close">Close</button>
        </div>
        ${runs.length ? rows.join('') : '<div class="empty-state">This task has not run yet.</div>'}
      </div>`;
    document.getElementById('history-close').addEventListener('click', () => { container.innerHTML = ''; });
  } catch (err) {
    container.innerHTML = `<div class="card" style="margin:16px 0"><p class="error">Failed to load history: ${esc(err.message)}</p></div>`;
  }
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

views.skills = async (content) => {
  const [installed, catalog] = await Promise.all([
    api('GET', '/skills'),
    api('GET', '/skills/catalog'),
  ]);
  const installedIds = new Set(installed.items.map((s) => s.id));
  const availableBuiltins = catalog.items.filter((b) => !installedIds.has(b.id));

  content.innerHTML = `
    <h2 class="section-title">Skills</h2>
    ${availableBuiltins.length ? `
      <h3 class="section-title" style="font-size:1rem">Available built-in skills</h3>
      <div class="grid" style="margin-bottom:20px">${availableBuiltins.map((b) => `
        <div class="card" data-builtin="${esc(b.id)}">
          <h3>${esc(b.id.replace('-skill', '').replace(/(^|\\b)(\\w)/g, (m) => m.toUpperCase()))}</h3>
          <p class="muted" style="margin-bottom:10px; font-size:.9rem">${esc(b.description)}</p>
          <button class="btn btn-sm" data-install="${esc(b.id)}">Install</button>
        </div>`).join('')}</div>` : ''}
    <div class="card" style="margin-bottom:16px">
      <h3>Install a custom skill package</h3>
      <p class="muted" style="margin-bottom:12px">Upload a skill ZIP. It is validated before installation.</p>
      <label class="file-drop" id="skill-drop" for="skill-file">
        <input type="file" id="skill-file" accept=".zip" hidden>
        <span id="skill-drop-text">📁 Choose a .zip file or drop it here</span>
      </label>
      <button class="btn hidden" id="skill-upload" style="margin-top:12px">Upload and install</button>
    </div>
    ${installed.items.length ? `<div class="grid">${installed.items.map((s) => `
      <div class="card" data-id="${esc(s.id)}">
        <h3>${esc(s.name)}</h3>
        <p class="muted" style="margin-bottom:8px">${esc(s.id)}</p>
        <span class="badge ${esc(s.status)}">${esc(s.status)}</span>
        <div style="margin-top:12px">
          <button class="btn btn-sm btn-secondary" data-action="enable">Enable</button>
          <button class="btn btn-sm btn-secondary" data-action="disable">Disable</button>
          <button class="btn btn-sm btn-danger" data-action="uninstall">Uninstall</button>
          <button class="btn btn-sm btn-secondary" data-action="versions">Versions</button>
        </div>
        <div class="skill-versions" style="margin-top:8px"></div>
      </div>`).join('')}</div>` : '<div class="empty-state">No skills installed yet.</div>'}
  `;

  // One-click builtin installation.
  content.querySelectorAll('[data-install]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const skillId = btn.dataset.install;
      btn.disabled = true;
      btn.textContent = 'Installing…';
      try {
        const { upload_id: uploadId } = await api('POST', `/skills/catalog/${skillId}/install`, {});
        const poll = setInterval(async () => {
          try {
            const status = await api('GET', `/skills/uploads/${uploadId}`);
            if (['installed', 'failed', 'cancelled'].includes(status.status)) {
              clearInterval(poll);
              if (status.status === 'installed') toast(`${skillId} installed`, 'success');
              else toast(`Installation failed: ${status.error_message || 'unknown error'}`, 'error');
              navigate('skills');
            }
          } catch (err) { clearInterval(poll); toast(err.message, 'error'); }
        }, 1000);
      } catch (err) {
        toast(err.message, 'error');
        btn.disabled = false;
        btn.textContent = 'Install';
      }
    });
  });

  // File picker: show the selected file and reveal the upload button only
  // after a file is chosen.
  const fileInput = document.getElementById('skill-file');
  const dropZone = document.getElementById('skill-drop');
  const dropText = document.getElementById('skill-drop-text');
  const uploadBtn = document.getElementById('skill-upload');
  const showSelected = (file) => {
    if (file) {
      dropText.textContent = `📄 ${file.name} (${(file.size / 1024).toFixed(1)} KB)`;
      dropZone.classList.add('has-file');
      uploadBtn.classList.remove('hidden');
    } else {
      dropText.textContent = '📁 Choose a .zip file or drop it here';
      dropZone.classList.remove('has-file');
      uploadBtn.classList.add('hidden');
    }
  };
  fileInput.addEventListener('change', () => showSelected(fileInput.files[0]));
  // Drag-and-drop support.
  dropZone.addEventListener('dragover', (e) => { e.preventDefault(); dropZone.classList.add('dragover'); });
  dropZone.addEventListener('dragleave', () => dropZone.classList.remove('dragover'));
  dropZone.addEventListener('drop', (e) => {
    e.preventDefault();
    dropZone.classList.remove('dragover');
    if (e.dataTransfer.files.length) {
      fileInput.files = e.dataTransfer.files;
      showSelected(e.dataTransfer.files[0]);
    }
  });

  uploadBtn.addEventListener('click', async () => {
    if (!fileInput.files.length) { toast('Select a ZIP file first', 'error'); return; }
    const form = new FormData();
    form.append('file', fileInput.files[0]);
    uploadBtn.disabled = true;
    uploadBtn.textContent = 'Installing…';
    try {
      const { upload_id: uploadId } = await api('POST', '/skills/uploads', form, true);
      toast('Upload accepted; validating…', 'success');
      // Poll the status endpoint until the pipeline finishes.
      const poll = setInterval(async () => {
        try {
          const status = await api('GET', `/skills/uploads/${uploadId}`);
          if (['installed', 'failed', 'cancelled'].includes(status.status)) {
            clearInterval(poll);
            if (status.status === 'installed') toast(`Skill ${status.skill_id} installed`, 'success');
            else toast(`Installation failed: ${status.error_message || 'unknown error'}`, 'error');
            navigate('skills');
          }
        } catch (err) {
          clearInterval(poll);
          toast(err.message, 'error');
          navigate('skills');
        }
      }, 1000);
    } catch (err) {
      toast(err.message, 'error');
      uploadBtn.disabled = false;
      uploadBtn.textContent = 'Upload and install';
    }
  });
  content.querySelectorAll('.card[data-id] button').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const skillId = btn.closest('.card').dataset.id;
      const action = btn.dataset.action;
      try {
        if (action === 'versions') {
          await showSkillVersions(skillId, btn.closest('.card'));
          return;
        }
        if (action === 'uninstall') {
          await api('DELETE', `/skills/${skillId}`);
        } else {
          await api('POST', `/skills/${skillId}/${action}`, {});
        }
        toast(`Skill ${action} done`, 'success');
        navigate('skills');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

/// Show a skill's installed versions with per-version delete (old versions
/// can be cleaned up; the active version is protected server-side).
async function showSkillVersions(skillId, card) {
  const container = card.querySelector('.skill-versions');
  container.innerHTML = '<span class="muted" style="font-size:.85rem">Loading…</span>';
  try {
    const { items } = await api('GET', `/skills/${skillId}/versions`);
    container.innerHTML = items.length ? items.map((v) => `
      <div class="skill-version-row" data-version="${esc(v.id)}">
        <span class="badge ${esc(v.status)}">${esc(v.version)}</span>
        <span class="muted" style="font-size:.8rem">${esc(v.installed_at.slice(0, 10))}</span>
        ${v.status !== 'active' ? `<button class="btn btn-sm btn-danger" data-delete-version="${esc(v.id)}">Delete</button>` : '<span class="badge active">in use</span>'}
      </div>`).join('') : '<span class="muted" style="font-size:.85rem">no versions</span>';
    container.querySelectorAll('[data-delete-version]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        if (!confirm('Delete this version?')) return;
        try {
          await api('DELETE', `/skills/${skillId}/versions/${btn.dataset.deleteVersion}`);
          toast('Version deleted', 'success');
          showSkillVersions(skillId, card);
        } catch (err) { toast(err.message, 'error'); }
      });
    });
  } catch (err) {
    container.innerHTML = `<span class="error" style="font-size:.85rem">${esc(err.message)}</span>`;
  }
}

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
      <div class="form-row" style="margin-top:10px; max-width:420px">
        <select id="integration-type">
          ${catalog.items.map((c) => `<option value="${esc(c.provider_type)}">${esc(c.provider_type)}</option>`).join('')}
        </select>
        <input id="integration-name" placeholder="Display name">
        <input id="integration-token" type="password" placeholder="Access token (for manual connection; leave empty to use OAuth when available)">
        <button class="btn" id="integration-connect">Connect</button>
      </div>
      <p class="muted" style="font-size:.85rem; margin-top:8px">Google providers use OAuth when GOOGLE_CLIENT_ID is configured; otherwise connect manually with an access token.</p>
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
    const token = document.getElementById('integration-token').value.trim();
    if (!displayName) { toast('Enter a display name', 'error'); return; }
    try {
      const result = await api('POST', `/integrations/${providerType}/connect`, {
        display_name: displayName,
        credentials: token ? { access_token: token } : {},
      });
      if (result.oauth_url) {
        // Real OAuth flow: redirect to Google's consent screen.
        window.location.href = result.oauth_url;
        return;
      }
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
  const { items } = await api('GET', '/approvals?status=pending&limit=100');
  content.innerHTML = `
    <h2 class="section-title">Approval inbox</h2>
    ${items.length ? items.map((a) => `
      <div class="card" style="margin-bottom:12px" data-id="${esc(a.id)}">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:8px">
          <span class="badge ${esc(a.status)}">${esc(a.status)}</span>
          ${a.tool_name ? `<code>${esc(a.tool_name)}</code>` : ''}
          <span class="muted">expires ${esc(a.expires_at)}</span>
        </div>
        ${a.args ? `<pre class="tool-trace-params" style="margin:0 0 10px">${esc(JSON.stringify(a.args, null, 2))}</pre>` : ''}
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
        refreshApprovalsBadge();
        navigate('approvals');
      } catch (err) { toast(err.message, 'error'); }
    });
  });
};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

views.settings = async (content) => {
  const [providers, workspaceConfig, agents, autoApprove] = await Promise.all([
    api('GET', '/llm/providers'),
    api('GET', '/workspace/config'),
    api('GET', '/agents'),
    api('GET', '/workspace/auto-approve'),
  ]);

  // Load models for each provider card.
  const modelsByProvider = {};
  await Promise.all(providers.items.map(async (p) => {
    try {
      const { items } = await api('GET', `/llm/providers/${p.id}/models`);
      modelsByProvider[p.id] = items;
    } catch { modelsByProvider[p.id] = []; }
  }));

  content.innerHTML = `
    <h2 class="section-title">Agents</h2>
    <p class="muted" style="margin-bottom:12px; font-size:.9rem">Agents are managed by the system: the assistant routes your requests automatically. You only need to touch them here to change instructions or remove one.</p>
    ${agents.items.map((a) => `
      <div class="card" style="margin-bottom:10px" data-agent="${esc(a.id)}">
        <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap">
          <strong>${esc(a.name)}</strong>
          <span class="badge ${esc(a.status)}">${esc(a.status)}</span>
          <span class="spacer"></span>
          <button class="btn btn-sm btn-secondary" data-action="edit-agent">Edit instructions</button>
          <button class="btn btn-sm btn-danger" data-action="delete-agent">Remove</button>
        </div>
        <div class="muted" style="font-size:.85rem; margin-top:6px; white-space:pre-wrap">${esc(a.instructions || 'No instructions')}</div>
      </div>`).join('')}

    <h2 class="section-title">Executions</h2>
    <div class="card" style="margin-bottom:16px">
      <label style="display:flex; align-items:center; gap:10px; cursor:pointer">
        <input type="checkbox" id="auto-approve-global" ${autoApprove.enabled ? 'checked' : ''}>
        <span>Run all tool executions without asking for approval</span>
      </label>
      <p class="muted" style="margin:8px 0 0; font-size:.85rem">When enabled, the agent executes tools immediately. Critical-risk operations still always ask. You can also override this per chat in the chat toolbar.</p>
    </div>

    <h2 class="section-title">LLM providers</h2>
    <div class="card" style="margin-bottom:16px">
      <div class="form-row" style="max-width:420px">
        <input id="provider-name" placeholder="Name">
        <select id="provider-type">
          <option value="openai_compatible">OpenAI-compatible</option>
          <option value="ollama">Ollama</option>
        </select>
        <input id="provider-url" placeholder="Base URL (https://… or http://localhost:11434 for Ollama)">
        <input id="provider-key" type="password" placeholder="API key (optional)">
        <button class="btn" id="provider-add">Add provider</button>
      </div>
    </div>
    ${providers.items.length ? providers.items.map((p) => `
      <div class="card" style="margin-bottom:12px" data-id="${esc(p.id)}">
        <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap">
          <strong>${esc(p.name)}</strong>
          <span class="muted">${esc(p.provider_type)} · ${esc(p.base_url)}</span>
          <span class="badge ${esc(p.status)}">${esc(p.status)}</span>
          <span class="spacer"></span>
          <button class="btn btn-sm btn-secondary" data-action="test">Test</button>
          <button class="btn btn-sm btn-danger" data-action="delete">Delete</button>
        </div>
        <div class="models-row" style="margin-top:10px">
          <strong style="font-size:.85rem">Models:</strong>
          ${(modelsByProvider[p.id] || []).map((m) => `
            <span class="badge active">${esc(m.model_name)}</span>`).join('') || '<span class="muted" style="font-size:.85rem">none</span>'}
          <div class="form-row" style="flex-direction:row; gap:8px; margin-top:8px; max-width:520px">
            <input class="model-name-input" placeholder="Model name (or discover below)">
            <button class="btn btn-sm" data-action="add-model">Add model</button>
            <button class="btn btn-sm btn-secondary" data-action="discover">Discover models</button>
          </div>
        </div>
      </div>`).join('') : '<p class="muted">No providers configured. The app works without one, but the agent needs an LLM to plan tasks.</p>'}
    <h2 class="section-title">MCP servers</h2>
    <div class="card" style="margin-bottom:16px">
      <div class="form-row" style="max-width:520px">
        <input id="mcp-name" placeholder="Server name">
        <input id="mcp-url" placeholder="MCP endpoint URL (https://…/mcp)">
        <input id="mcp-auth" type="password" placeholder="Authorization header value (optional)">
        <button class="btn" id="mcp-add">Connect MCP server</button>
      </div>
    </div>
    <div id="mcp-list"></div>
    <h2 class="section-title">Workspace</h2>
    <div class="card"><pre style="font-size:.85rem">${esc(JSON.stringify(workspaceConfig, null, 2))}</pre></div>
  `;

  // Render MCP servers with their tools.
  const mcpList = document.getElementById('mcp-list');
  try {
    const { items: mcpServers } = await api('GET', '/mcp-servers');
    if (mcpServers.length) {
      mcpList.innerHTML = mcpServers.map((s) => `
        <div class="card" style="margin-bottom:10px" data-mcp="${esc(s.id)}">
          <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap">
            <strong>${esc(s.name)}</strong>
            <span class="muted" style="font-size:.85rem">${esc(s.url)}</span>
            <span class="badge ${s.enabled ? 'enabled' : 'disabled'}">${s.enabled ? 'enabled' : 'disabled'}</span>
            <span class="spacer"></span>
            <button class="btn btn-sm btn-secondary" data-action="list-tools">List tools</button>
            <button class="btn btn-sm btn-danger" data-action="delete-mcp">Remove</button>
          </div>
          <div class="mcp-tools" style="margin-top:8px"></div>
        </div>`).join('');
      mcpList.querySelectorAll('.card[data-mcp]').forEach((card) => {
        const serverId = card.dataset.mcp;
        card.querySelectorAll('button').forEach((btn) => {
          btn.addEventListener('click', async () => {
            try {
              if (btn.dataset.action === 'list-tools') {
                const { items: tools } = await api('GET', `/mcp-servers/${serverId}/tools`);
                card.querySelector('.mcp-tools').innerHTML = tools.length
                  ? tools.map((t) => `<span class="badge active" title="${esc(t.description)}">${esc(t.name)}</span>`).join(' ')
                  : '<span class="muted" style="font-size:.85rem">no tools</span>';
              } else if (btn.dataset.action === 'delete-mcp') {
                if (!confirm('Remove this MCP server?')) return;
                await api('DELETE', `/mcp-servers/${serverId}`);
                toast('MCP server removed', 'success');
                navigate('settings');
              }
            } catch (err) { toast(err.message, 'error'); }
          });
        });
      });
    } else {
      mcpList.innerHTML = '<p class="muted" style="font-size:.9rem">No MCP servers connected. Connect one to extend the agent with external tools.</p>';
    }
  } catch { /* MCP API unavailable */ }
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
  document.getElementById('auto-approve-global').addEventListener('change', async (e) => {
    try {
      await api('PUT', '/workspace/auto-approve', { enabled: e.target.checked });
      toast(e.target.checked ? 'Auto-approve enabled' : 'Auto-approve disabled', 'success');
    } catch (err) {
      toast(err.message, 'error');
      e.target.checked = !e.target.checked;
    }
  });
  // Agent management.
  content.querySelectorAll('.card[data-agent]').forEach((card) => {
    const agentId = card.dataset.agent;
    card.querySelectorAll('button').forEach((btn) => {
      btn.addEventListener('click', async () => {
        try {
          if (btn.dataset.action === 'edit-agent') {
            const agent = agents.items.find((a) => a.id === agentId);
            const instructions = prompt('Agent instructions', agent?.instructions || '');
            if (instructions === null) return;
            await api('PATCH', `/agents/${agentId}`, { instructions });
            toast('Agent updated', 'success');
            navigate('settings');
          } else if (btn.dataset.action === 'delete-agent') {
            if (!confirm('Remove this agent?')) return;
            await api('DELETE', `/agents/${agentId}`);
            toast('Agent removed', 'success');
            navigate('settings');
          }
        } catch (err) { toast(err.message, 'error'); }
      });
    });
  });
  // MCP server connect.
  document.getElementById('mcp-add').addEventListener('click', async () => {
    const name = document.getElementById('mcp-name').value.trim();
    const url = document.getElementById('mcp-url').value.trim();
    const auth = document.getElementById('mcp-auth').value.trim();
    if (!name || !url) { toast('Name and URL are required', 'error'); return; }
    try {
      await api('POST', '/mcp-servers', { name, url, auth_header: auth ? `Bearer ${auth}` : null });
      toast('MCP server connected', 'success');
      navigate('settings');
    } catch (err) { toast(err.message, 'error'); }
  });
  content.querySelectorAll('.card[data-id]').forEach((card) => {
    const providerId = card.dataset.id;
    card.querySelectorAll('button').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const action = btn.dataset.action;
        try {
          if (action === 'test') {
            const result = await api('POST', `/llm/providers/${providerId}/test`, {});
            toast(result.reachable ? `Provider reachable (HTTP ${result.http_status})` : 'Provider unreachable', result.reachable ? 'success' : 'error');
          } else if (action === 'delete') {
            if (!confirm('Delete this provider?')) return;
            await api('DELETE', `/llm/providers/${providerId}`);
            toast('Provider deleted', 'success');
            navigate('settings');
          } else if (action === 'add-model') {
            const modelName = card.querySelector('.model-name-input').value.trim();
            if (!modelName) { toast('Enter a model name', 'error'); return; }
            await api('POST', `/llm/providers/${providerId}/models`, { model_name: modelName });
            toast('Model added', 'success');
            navigate('settings');
          } else if (action === 'discover') {
            btn.disabled = true;
            btn.textContent = 'Discovering…';
            try {
              const result = await api('POST', `/llm/providers/${providerId}/models/discover`, {});
              toast(`Discovered ${result.discovered} model(s), ${result.added} new`, 'success');
              navigate('settings');
            } catch (err) {
              toast(err.message, 'error');
              btn.disabled = false;
              btn.textContent = 'Discover models';
            }
          }
        } catch (err) { toast(err.message, 'error'); }
      });
    });
  });
};

function updateChatSidebar(title) {
  const list = document.getElementById('chat-list');
  if (!list || !currentConversationId) return;
  let current = list.querySelector(`.chat-list-item[data-conv="${currentConversationId}"]`);
  if (!current) {
    // New conversation: insert a list item at the top.
    current = document.createElement('div');
    current.className = 'chat-list-item';
    current.dataset.conv = currentConversationId;
    current.innerHTML = '<div class="chat-list-title"></div><div class="chat-list-meta muted"></div>';
    current.addEventListener('click', () => {
      currentConversationId = current.dataset.conv;
      sessionStorage.setItem('auren_conv', currentConversationId);
      navigate('chat');
    });
    list.prepend(current);
  } else {
    list.prepend(current);
  }
  if (title) current.querySelector('.chat-list-title').textContent = title;
  const now = new Date().toISOString().slice(0, 16).replace('T', ' ');
  current.querySelector('.chat-list-meta').textContent = now;
  list.querySelectorAll('.chat-list-item').forEach((i) =>
    i.classList.toggle('selected', i.dataset.conv === currentConversationId));
}

function addBubble(role, text) {
  const container = document.getElementById('chat-messages');
  const bubble = document.createElement('div');
  bubble.className = `chat-bubble ${role}`;
  if (role === 'assistant' && window.marked) {
    // Assistant messages render as markdown; user messages stay plain text.
    bubble.innerHTML = marked.parse(text ?? '');
  } else {
    bubble.textContent = text;
  }
  container.appendChild(bubble);
  container.scrollTop = container.scrollHeight;
  return bubble;
}

/// Render an expandable tool-call trace under an assistant bubble. Calls
/// with status "approval_required" render an inline decision card with the
/// tool name and arguments, so the user can approve without leaving the chat.
function addToolTrace(bubble, trace) {
  if (!trace || !trace.length) return;
  const details = document.createElement('details');
  details.className = 'tool-trace';
  const summary = document.createElement('summary');
  summary.textContent = `🔧 ${trace.length} tool call${trace.length > 1 ? 's' : ''}`;
  details.appendChild(summary);
  for (const call of trace) {
    const item = document.createElement('div');
    item.className = 'tool-trace-item';
    if (call.status === 'approval_required' && call.approval_id) {
      item.innerHTML = `<div class="tool-trace-head">⏳ <code>${esc(call.tool)}</code> — waiting for your approval</div>`;
      const params = document.createElement('pre');
      params.className = 'tool-trace-params';
      params.textContent = JSON.stringify(call.arguments, null, 2);
      item.appendChild(params);
      const actions = document.createElement('div');
      actions.className = 'approval-inline-actions';
      const approve = document.createElement('button');
      approve.className = 'btn btn-sm';
      approve.textContent = 'Approve';
      const reject = document.createElement('button');
      reject.className = 'btn btn-sm btn-danger';
      reject.textContent = 'Reject';
      const decide = async (action) => {
        approve.disabled = true;
        reject.disabled = true;
        try {
          const detail = await api('GET', `/approvals/${call.approval_id}`);
          await api('POST', `/approvals/${call.approval_id}/${action}`, { operation: detail.operation });
          item.querySelector('.tool-trace-head').innerHTML =
            `${action === 'approve' ? '✓' : '✗'} <code>${esc(call.tool)}</code> — ${action}d`;
          refreshApprovalsBadge();
          // The chat exchange stopped at the approval; resuming runs the
          // approved tool and continues the agent from there.
          if (action === 'approve' && currentConversationId) {
            const pending = addBubble('assistant', '…');
            try {
              const result = await api('POST', `/conversations/${currentConversationId}/resume`, { approval_id: call.approval_id });
              pending.remove();
              const replyBubble = addBubble('assistant', result.reply);
              if (result.error) replyBubble.classList.add('error-bubble');
              addToolTrace(replyBubble, result.tool_trace);
            } catch (err) {
              pending.textContent = `Error: ${err.message}`;
              pending.classList.add('error-bubble');
            }
          }
        } catch (err) {
          toast(err.message, 'error');
          approve.disabled = false;
          reject.disabled = false;
        }
      };
      approve.addEventListener('click', () => decide('approve'));
      reject.addEventListener('click', () => decide('reject'));
      actions.appendChild(approve);
      actions.appendChild(reject);
      item.appendChild(actions);
      details.appendChild(item);
      continue;
    }
    const statusIcon = call.status === 'ok' ? '✓' : '✗';
    item.innerHTML = `<div class="tool-trace-head">${statusIcon} <code>${esc(call.tool)}</code></div>`;
    const params = document.createElement('pre');
    params.className = 'tool-trace-params';
    params.textContent = JSON.stringify(call.arguments, null, 2);
    item.appendChild(params);
    if (call.error) {
      const err = document.createElement('div');
      err.className = 'tool-trace-error';
      err.textContent = call.error;
      item.appendChild(err);
    }
    details.appendChild(item);
  }
  // The trace belongs to its reply bubble, not to the end of the
  // transcript: appending to the container made it drift to the bottom
  // (and duplicate visually) as more messages arrived.
  bubble.insertAdjacentElement('afterend', details);
  document.getElementById('chat-messages').scrollTop = document.getElementById('chat-messages').scrollHeight;
}

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