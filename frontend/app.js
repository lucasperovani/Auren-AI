// Auren-AI web frontend (spec section 5).
// Vanilla ES module SPA served by the same application server as the API.

const API = '/api/v1';
let sessionToken = sessionStorage.getItem('auren_token') || '';
let currentUser = null;
let ws = null;
let wsReconnectDelay = 1000;
// Model cache for the chat model picker, refreshed per render.
let chatModels = {};

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
// Formatting helpers: users never see raw cron expressions or ISO timestamps.
// ---------------------------------------------------------------------------

const DAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const DAY_SHORT = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const MONTH_NAMES = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
];

const pad2 = (n) => String(n).padStart(2, '0');

const dateTimeFmt = new Intl.DateTimeFormat('en', {
  month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', hour12: false,
});
const clockFmt = new Intl.DateTimeFormat('en', {
  hour: '2-digit', minute: '2-digit', hour12: false,
});
const relativeFmt = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

/** Parse a cron field into a number, or null when it is not a plain value. */
function cronInt(value) {
  return /^\d+$/.test(value) ? Number(value) : null;
}

/** Step of a `*\/n` field, or null. */
function cronStep(value) {
  const match = /^\*\/(\d+)$/.exec(value ?? '');
  return match ? Number(match[1]) : null;
}

/** Ordinal suffix: 1 -> 1st, 22 -> 22nd. */
function ordinal(n) {
  const rem100 = n % 100;
  if (rem100 >= 11 && rem100 <= 13) return `${n}th`;
  return `${n}${['th', 'st', 'nd', 'rd'][n % 10] ?? 'th'}`;
}

/** Expand `1-5` or `1,3,5` into day numbers; null when not a day list. */
function cronDayList(field) {
  const parts = field.split(',');
  const days = [];
  for (const part of parts) {
    if (part.includes('-')) {
      const [start, end] = part.split('-').map(Number);
      if (Number.isNaN(start) || Number.isNaN(end) || end < start) return null;
      for (let d = start; d <= end; d += 1) days.push(d % 7);
    } else {
      const day = cronInt(part);
      if (day === null) return null;
      days.push(day % 7);
    }
  }
  return days.length ? [...new Set(days)].sort((a, b) => a - b) : null;
}

/** "Mon, Wed and Fri" for a set of weekday numbers. */
function describeDays(days) {
  const names = days.map((d) => DAY_SHORT[d]);
  if (names.length === 1) return names[0];
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/**
 * Turn a 5-field cron expression into plain English. Falls back to a generic
 * label for expressions outside the supported subset, so the raw expression
 * is never surfaced as the primary description.
 */
function describeCron(expression) {
  if (!expression) return 'Manual only';
  const fields = String(expression).trim().split(/\s+/);
  if (fields.length !== 5) return 'Custom schedule';
  const [minute, hour, dom, month, dow] = fields;

  const minuteValue = cronInt(minute);
  const hourValue = cronInt(hour);
  const minuteStep = cronStep(minute);
  const hourStep = cronStep(hour);
  const time = (h, m = minuteValue) => `${pad2(h)}:${pad2(m)}`;
  const daily = dom === '*' && month === '*' && dow === '*';

  // Intra-hour and hourly patterns.
  if (hour === '*' && daily) {
    if (minute === '*' || minuteStep === 1) return 'Every minute';
    if (minuteStep) return `Every ${minuteStep} minutes`;
    if (minuteValue === 0) return 'Every hour, on the hour';
    if (minuteValue !== null) return `Every hour at :${pad2(minuteValue)}`;
    return 'Every hour';
  }

  // Sub-daily step patterns such as `0 */6 * * *`.
  if (hourStep && minuteValue !== null && daily) {
    return hourStep === 1
      ? `Every hour${minuteValue ? ` at :${pad2(minuteValue)}` : ', on the hour'}`
      : `Every ${hourStep} hours`;
  }

  if (minuteValue === null || hourValue === null) return 'Custom schedule';

  if (daily) return `Every day at ${time(hourValue)}`;

  // Weekly / weekday patterns.
  if (dom === '*' && month === '*') {
    const days = cronDayList(dow);
    if (days) {
      if (days.length === 5 && days.join() === '1,2,3,4,5') return `Weekdays at ${time(hourValue)}`;
      if (days.length === 2 && days.join() === '0,6') return `Weekends at ${time(hourValue)}`;
      if (days.length === 1) return `Every ${DAY_NAMES[days[0]]} at ${time(hourValue)}`;
      return `Every ${describeDays(days)} at ${time(hourValue)}`;
    }
  }

  // Monthly / yearly patterns.
  const domValue = cronInt(dom);
  if (domValue !== null && dow === '*') {
    if (month === '*') return `Monthly on the ${ordinal(domValue)} at ${time(hourValue)}`;
    const monthValue = cronInt(month);
    if (monthValue !== null && monthValue >= 1 && monthValue <= 12) {
      return `Every year on ${MONTH_NAMES[monthValue - 1]} ${domValue} at ${time(hourValue)}`;
    }
  }

  return 'Custom schedule';
}

/** Short absolute timestamp, e.g. "Nov 10, 14:32". */
function formatDateTime(iso) {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '—';
  return dateTimeFmt.format(date);
}

/** Clock time only, e.g. "14:32". */
function formatClock(iso) {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '—';
  return clockFmt.format(date);
}

/** Relative description such as "in 3 hours" or "2 days ago". */
function relativeTime(iso) {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  const seconds = Math.round((date.getTime() - Date.now()) / 1000);
  const abs = Math.abs(seconds);
  if (abs < 45) return relativeFmt.format(0, 'second');
  if (abs < 2700) return relativeFmt.format(Math.round(seconds / 60), 'minute');
  if (abs < 79200) return relativeFmt.format(Math.round(seconds / 3600), 'hour');
  if (abs < 2592000) return relativeFmt.format(Math.round(seconds / 86400), 'day');
  if (abs < 31536000) return relativeFmt.format(Math.round(seconds / 2592000), 'month');
  return relativeFmt.format(Math.round(seconds / 31536000), 'year');
}

/** Duration between two instants, e.g. "4.2s" or "1m 12s". */
function formatDuration(startIso, endIso) {
  if (!startIso) return '';
  const start = new Date(startIso).getTime();
  const end = endIso ? new Date(endIso).getTime() : Date.now();
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return '';
  const totalSeconds = (end - start) / 1000;
  if (totalSeconds < 1) return `${Math.round(totalSeconds * 1000)}ms`;
  if (totalSeconds < 60) return `${totalSeconds.toFixed(1)}s`;
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = Math.round(totalSeconds % 60);
  return `${minutes}m ${seconds}s`;
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
  refreshNotificationBadge();
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
    const severity = envelope.data?.severity || 'info';
    const label = severity === 'error' ? 'Something needs attention'
      : severity === 'warning' ? 'Check your notifications'
        : 'New notification';
    toast(label, severity === 'error' ? 'error' : 'success');
    refreshNotificationBadge();
  } else if (envelope.event_type === 'task_run.approval_required') {
    toast('An operation requires your approval', 'error');
    refreshApprovalsBadge();
  } else if (envelope.event_type === 'approval.updated') {
    refreshApprovalsBadge();
  } else if (envelope.event_type === 'notification.acted') {
    refreshNotificationBadge();
  } else if (envelope.event_type.startsWith('task.')) {
    // Refresh the current view when task state changes.
    navigate(location.hash.replace('#', '') || 'dashboard');
  }
}

/// Unread notifications badge on the sidebar.
async function refreshNotificationBadge() {
  const badge = document.getElementById('notifications-badge');
  if (!badge) return;
  try {
    const { unread } = await api('GET', '/notifications/unread-count');
    badge.textContent = unread > 99 ? '99+' : String(unread);
    badge.classList.toggle('hidden', !unread);
  } catch { /* badge is best-effort */ }
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
    api('GET', '/tasks?limit=100'),
    api('GET', '/skills'),
    api('GET', '/integrations'),
    api('GET', '/approvals?status=pending'),
    api('GET', '/notifications?limit=5'),
  ]);

  const activeTasks = tasks.items.filter((t) => t.status === 'active').length;
  const scheduled = tasks.items.filter((t) => t.schedule_cron && t.status === 'active').length;
  const failing = tasks.items.filter((t) => t.last_run_status === 'failed').length;
  const recent = [...tasks.items]
    .sort((a, b) => (b.updated_at || '').localeCompare(a.updated_at || ''))
    .slice(0, 5);

  content.innerHTML = `
    ${pageHead('Dashboard', `Signed in as ${currentUser?.display_name || 'you'}.`)}
    <div class="grid">
      <div class="card stat-card interactive">
        <div class="stat-icon">◷</div>
        <div class="stat stat-accent">${activeTasks}</div>
        <div class="stat-label">Active tasks${scheduled ? ` · ${scheduled} scheduled` : ''}</div>
      </div>
      <div class="card stat-card interactive">
        <div class="stat-icon">✓</div>
        <div class="stat ${approvals.items.length ? 'stat-warning' : ''}">${approvals.items.length}</div>
        <div class="stat-label">Pending approvals</div>
      </div>
      <div class="card stat-card interactive">
        <div class="stat-icon">◆</div>
        <div class="stat stat-success">${skills.items.length}</div>
        <div class="stat-label">Installed skills</div>
      </div>
      <div class="card stat-card interactive">
        <div class="stat-icon">⇄</div>
        <div class="stat ${failing ? 'stat-danger' : ''}">${integrations.items.length}</div>
        <div class="stat-label">Integrations${failing ? ` · ${failing} failing` : ''}</div>
      </div>
    </div>

    <div class="section-head">
      <h3 class="section-title">Recent tasks</h3>
      <span class="spacer"></span>
      <a href="#tasks" class="btn btn-sm btn-secondary">View all</a>
    </div>
    ${recent.length ? `<table class="table">
      <thead><tr><th>Task</th><th>Status</th><th>Runs</th><th>Updated</th></tr></thead>
      <tbody>${recent.map((t) => `
        <tr>
          <td><a href="#tasks">${esc(t.title)}</a>
            <div class="muted" style="font-size:.8rem">${esc(taskScheduleLabel(t))}</div></td>
          <td><span class="badge ${esc(t.status)}">${esc(t.status)}</span></td>
          <td class="muted">${esc(t.last_run_status || 'never')}</td>
          <td class="muted">${esc(relativeTime(t.updated_at))}</td>
        </tr>`).join('')}
      </tbody></table>`
      : emptyState('No tasks yet. Create one from the Tasks view or ask the agent in Chat.', '🗒')}

    <div class="section-head">
      <h3 class="section-title">Recent activity</h3>
      <span class="spacer"></span>
      <a href="#notifications" class="btn btn-sm btn-secondary">All notifications</a>
    </div>
    ${notifications.items.length ? notifications.items.map((n) => `
      <div class="card" style="margin-bottom:10px">
        <div style="display:flex;align-items:center;gap:10px">
          <span class="notif-sev ${severityMeta(n.severity).cls}" aria-hidden="true">${severityMeta(n.severity).icon}</span>
          <strong>${esc(n.title)}</strong>
          <span class="spacer"></span>
          <span class="muted" style="font-size:.8rem">${esc(relativeTime(n.created_at))}</span>
        </div>
        <div class="muted" style="font-size:.87rem;margin-top:4px">${esc(n.body || '')}</div>
      </div>`).join('') : emptyState('No notifications yet.', '✉')}
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
                  <div class="chat-list-meta muted">${esc(relativeTime(c.updated_at))}</div>
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

// ---------------------------------------------------------------------------
// Shared UI helpers
// ---------------------------------------------------------------------------

/** Page header with title, subtitle and right-aligned actions. */
function pageHead(title, subtitle, actionsHtml = '') {
  return `
    <div class="page-head">
      <div>
        <h2>${esc(title)}</h2>
        ${subtitle ? `<div class="page-sub">${esc(subtitle)}</div>` : ''}
      </div>
      ${actionsHtml ? `<div class="page-actions">${actionsHtml}</div>` : ''}
    </div>`;
}

/** Empty state placeholder with an icon and an optional call to action. */
function emptyState(message, icon = '✦', actionHtml = '') {
  return `<div class="empty-state">
    <div class="empty-icon">${icon}</div>
    <div>${esc(message)}</div>
    ${actionHtml}
  </div>`;
}

/** Open a modal dialog; resolves with the element when closed. */
function openModal({ title, bodyHtml, footerHtml }) {
  const backdrop = document.createElement('div');
  backdrop.className = 'modal-backdrop';
  backdrop.innerHTML = `
    <div class="modal" role="dialog" aria-modal="true" aria-label="${esc(title)}">
      <div class="modal-head">
        <h3>${esc(title)}</h3>
        <span class="spacer"></span>
        <button class="btn-icon" data-close aria-label="Close">✕</button>
      </div>
      <div class="modal-body">${bodyHtml}</div>
      ${footerHtml ? `<div class="modal-foot">${footerHtml}</div>` : ''}
    </div>`;
  const close = () => {
    backdrop.remove();
    document.removeEventListener('keydown', onKey);
  };
  const onKey = (event) => { if (event.key === 'Escape') close(); };
  backdrop.addEventListener('click', (event) => {
    if (event.target === backdrop || event.target.closest('[data-close]')) close();
  });
  document.addEventListener('keydown', onKey);
  document.body.appendChild(backdrop);
  return { root: backdrop, close };
}

/** Preset schedules offered in the task form: label, cron, help text. */
const SCHEDULE_PRESETS = [
  { label: 'Hourly', cron: '0 * * * *', hint: 'At the start of every hour' },
  { label: 'Daily', cron: '0 9 * * *', hint: 'Every day at 09:00' },
  { label: 'Weekdays', cron: '0 9 * * 1-5', hint: 'Monday to Friday at 09:00' },
  { label: 'Weekly', cron: '0 9 * * 1', hint: 'Every Monday at 09:00' },
  { label: 'Monthly', cron: '0 9 1 * *', hint: 'On the 1st of each month' },
];

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/** Board columns, in order. The stage is a workflow position; the execution
 *  status (active/paused/draft…) stays independent and keeps driving the
 *  scheduler. */
const KANBAN_COLUMNS = [
  {
    id: 'analysis',
    label: 'Analysis',
    hint: 'Needs scope or requirements',
    accent: 'col-analysis',
  },
  {
    id: 'todo',
    label: 'To Do',
    hint: 'Ready to run',
    accent: 'col-todo',
  },
  {
    id: 'in_progress',
    label: 'In Progress',
    hint: 'Running now',
    accent: 'col-progress',
  },
  {
    id: 'done',
    label: 'Done',
    hint: 'Finished work',
    accent: 'col-done',
  },
];

/** Status filters applied on top of the board. */
const TASK_FILTERS = [
  { id: 'all', label: 'All' },
  { id: 'scheduled', label: 'Scheduled', statuses: ['active'] },
  { id: 'manual', label: 'Manual', statuses: ['draft'] },
  { id: 'paused', label: 'Paused', statuses: ['paused'] },
];

let taskFilter = sessionStorage.getItem('auren_task_filter') || 'all';
// Board state survives navigation: which columns are collapsed.
let collapsedColumns = new Set(
  (sessionStorage.getItem('auren_kanban_collapsed') || '').split(',').filter(Boolean),
);

/** The schedule sentence shown on a task card. */
function taskScheduleLabel(task) {
  if (!task.schedule_cron) return 'Manual run only';
  return describeCron(task.schedule_cron);
}

/** One-line summary of when the task runs next / last ran. */
function taskTimingHtml(task) {
  const items = [];
  if (task.schedule_cron) {
    if (task.next_run_at && task.status === 'active') {
      const dueSoon = new Date(task.next_run_at).getTime() - Date.now() < 60 * 60 * 1000;
      items.push(`<span class="task-meta-item ${dueSoon ? 'is-due' : ''}">
        <span class="dot"></span><strong>Next</strong> ${esc(relativeTime(task.next_run_at))}
      </span>`);
    } else if (task.status === 'paused') {
      items.push(`<span class="task-meta-item is-idle"><span class="dot"></span>Paused</span>`);
    }
  }
  if (task.last_run_at) {
    const status = task.last_run_status || '';
    items.push(`<span class="task-meta-item">
      <span class="badge ${esc(status)} plain">${esc(status)}</span>
      <span>${esc(relativeTime(task.last_run_at))}</span>
    </span>`);
  } else {
    items.push('<span class="task-meta-item is-idle"><span class="dot"></span>Never ran</span>');
  }
  return items.join('');
}

views.tasks = async (content) => {
  const [agents, { items }] = await Promise.all([
    api('GET', '/agents'),
    api('GET', '/tasks?limit=200'),
  ]);

  const activeFilter = TASK_FILTERS.find((f) => f.id === taskFilter) || TASK_FILTERS[0];
  const visible = activeFilter.statuses
    ? items.filter((t) => activeFilter.statuses.includes(t.status))
    : items;

  const counts = TASK_FILTERS.map((f) => ({
    ...f,
    count: f.statuses ? items.filter((t) => f.statuses.includes(t.status)).length : items.length,
  }));

  const openForm = (task) => showTaskForm(task, agents.items);

  content.innerHTML = `
    ${pageHead(
      'Tasks',
      'Drag cards between columns to track work. Columns are independent of scheduling: a scheduled task keeps firing wherever it sits.',
      '<button class="btn" id="task-new">+ New task</button>',
    )}
    <div class="task-toolbar">
      ${counts.map((f) => `
        <button class="filter-chip ${f.id === activeFilter.id ? 'active' : ''}" data-filter="${f.id}">
          ${esc(f.label)}${f.count ? ` · ${f.count}` : ''}
        </button>`).join('')}
      <span class="spacer"></span>
      <button class="btn btn-sm btn-secondary" id="kanban-expand-all">Expand all</button>
    </div>
    <div class="kanban" id="kanban">
      ${KANBAN_COLUMNS.map((col) => kanbanColumnHtml(col, visible.filter((t) => (t.stage || 'todo') === col.id))).join('')}
    </div>
  `;

  // Filter chips.
  content.querySelectorAll('[data-filter]').forEach((chip) => {
    chip.addEventListener('click', () => {
      taskFilter = chip.dataset.filter;
      sessionStorage.setItem('auren_task_filter', taskFilter);
      navigate('tasks');
    });
  });

  document.getElementById('task-new').addEventListener('click', () => openForm(null));

  content.querySelectorAll('[data-kanban-new]').forEach((btn) => {
    btn.addEventListener('click', () => openForm(null));
  });

  // Column collapse toggles.
  content.querySelectorAll('[data-collapse]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const col = btn.dataset.collapse;
      if (collapsedColumns.has(col)) collapsedColumns.delete(col);
      else collapsedColumns.add(col);
      sessionStorage.setItem('auren_kanban_collapsed', [...collapsedColumns].join(','));
      navigate('tasks');
    });
  });

  const expandAll = document.getElementById('kanban-expand-all');
  if (expandAll) {
    expandAll.addEventListener('click', () => {
      collapsedColumns.clear();
      sessionStorage.setItem('auren_kanban_collapsed', '');
      navigate('tasks');
    });
  }

  // Per-card actions.
  content.querySelectorAll('.kanban-card').forEach((card) => {
    const task = items.find((t) => t.id === card.dataset.id);
    if (!task) return;
    card.querySelectorAll('[data-action]').forEach((btn) => {
      btn.addEventListener('click', async (event) => {
        // Buttons inside a draggable card must not start a drag.
        event.stopPropagation();
        const action = btn.dataset.action;
        if (action === 'edit') return openForm(task);
        if (action === 'history') return showRunHistory(task);

        if (action === 'delete') {
          if (!confirm(`Delete "${task.title}"? Its run history is removed too.`)) return;
          try {
            await api('DELETE', `/tasks/${task.id}`);
            toast('Task deleted', 'success');
            navigate('tasks');
          } catch (err) { toast(err.message, 'error'); }
          return;
        }

        btn.disabled = true;
        try {
          await api('POST', `/tasks/${task.id}/${action}`, {});
          const messages = {
            run: 'Run queued',
            pause: 'Task paused',
            resume: 'Task resumed',
            cancel: 'Task cancelled',
          };
          toast(messages[action] || `Task ${action} done`, 'success');
          navigate('tasks');
        } catch (err) {
          toast(err.message, 'error');
          btn.disabled = false;
        }
      });
    });
  });

  wireKanbanDrag(content, items);
};

/** Markup for one board column and the cards it holds. */
function kanbanColumnHtml(col, cards) {
  const collapsed = collapsedColumns.has(col.id);
  return `
    <section class="kanban-col ${col.accent} ${collapsed ? 'is-collapsed' : ''}" data-col="${col.id}">
      <header class="kanban-col-head">
        <span class="kanban-dot" aria-hidden="true"></span>
        <h3>${esc(col.label)}</h3>
        <span class="kanban-count">${cards.length}</span>
        <span class="spacer"></span>
        <button class="btn-icon" data-collapse="${col.id}"
          title="${collapsed ? 'Expand' : 'Collapse'} ${esc(col.label)}">${collapsed ? '▸' : '▾'}</button>
      </header>
      ${collapsed ? '' : `
        <div class="kanban-col-hint muted">${esc(col.hint)}</div>
        <div class="kanban-drop" data-drop="${col.id}">
          ${cards.length
            ? cards.map((t) => kanbanCardHtml(t)).join('')
            : `<div class="kanban-empty">Drop a task here</div>`}
        </div>
        <button class="kanban-add" data-kanban-new>+ Add task</button>
      `}
    </section>`;
}

/** Card markup for one task inside a board column. */
function kanbanCardHtml(task) {
  const isRunning = task.last_run_status === 'running' || task.last_run_status === 'queued';
  const canPause = task.status === 'active';
  const canResume = task.status === 'paused';
  const closed = task.stage === 'done';
  return `
    <article class="kanban-card ${closed ? 'is-done' : ''}" draggable="true" data-id="${esc(task.id)}"
      tabindex="0" role="group"
      aria-label="${esc(task.title)} — stage ${esc(task.stage || 'todo')}"> 
      <div class="kanban-card-head">
        <div class="task-title">${esc(task.title)}</div>
        <span class="spacer"></span>
        <span class="badge ${esc(task.status)} plain">${esc(task.status)}</span>
      </div>
      <div class="task-objective">${esc(task.objective)}</div>
      <div class="task-meta">
        <span class="task-meta-item"><span class="dot"></span>${esc(taskScheduleLabel(task))}</span>
        ${taskTimingHtml(task)}
      </div>
      <div class="task-actions">
        <button class="btn btn-sm" data-action="run" ${isRunning ? 'disabled' : ''}>Run now</button>
        ${canPause ? '<button class="btn btn-sm btn-secondary" data-action="pause">Pause</button>' : ''}
        ${canResume ? '<button class="btn btn-sm btn-secondary" data-action="resume">Resume</button>' : ''}
        <button class="btn btn-sm btn-secondary" data-action="history" title="Run history">History</button>
        <button class="btn btn-sm btn-secondary" data-action="edit" title="Edit task">Edit</button>
        <span class="spacer"></span>
        <button class="btn-icon danger" data-action="delete" title="Delete task">✕</button>
      </div>
    </article>`;
}

/**
 * Drag and drop between columns. HTML5 drag events are used directly so no
 * library is needed, and a keyboard fallback (move menu) keeps the board usable
 * without a mouse.
 */
function wireKanbanDrag(content, tasks) {
  const board = document.getElementById('kanban');
  if (!board) return;
  let draggedId = null;

  const cards = content.querySelectorAll('.kanban-card');
  cards.forEach((card) => {
    card.addEventListener('dragstart', (event) => {
      draggedId = card.dataset.id;
      card.classList.add('is-dragging');
      event.dataTransfer.effectAllowed = 'move';
      // Firefox requires data to be set for the drag to start.
      event.dataTransfer.setData('text/plain', draggedId);
    });
    card.addEventListener('dragend', () => {
      card.classList.remove('is-dragging');
      draggedId = null;
      content.querySelectorAll('.kanban-drop').forEach((drop) => drop.classList.remove('is-over'));
    });
  });

  content.querySelectorAll('[data-drop]').forEach((drop) => {
    const column = drop.dataset.drop;

    drop.addEventListener('dragover', (event) => {
      if (!draggedId) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
      drop.classList.add('is-over');
    });
    drop.addEventListener('dragleave', () => drop.classList.remove('is-over'));

    drop.addEventListener('drop', async (event) => {
      event.preventDefault();
      drop.classList.remove('is-over');
      const taskId = event.dataTransfer.getData('text/plain') || draggedId;
      if (!taskId) return;
      const task = tasks.find((t) => t.id === taskId);
      if (!task || task.stage === column) return;

      // Optimistic move: the card follows the cursor immediately. On failure
      // the board is refetched, so a rejected move is never shown as applied.
      const card = content.querySelector(`.kanban-card[data-id="${taskId}"]`);
      if (card) drop.appendChild(card);

      try {
        await api('POST', `/tasks/${taskId}/stage`, { stage: column });
        toast(`Moved to ${KANBAN_COLUMNS.find((c) => c.id === column)?.label || column}`, 'success');
        navigate('tasks');
      } catch (err) {
        toast(err.message, 'error');
        navigate('tasks');
      }
    });
  });

  // Keyboard accessibility: the board must be usable without a mouse.
  // Arrow keys move the focused card between columns.
  content.querySelectorAll('.kanban-card').forEach((card) => {
    const task = tasks.find((t) => t.id === card.dataset.id);
    if (!task) return;
    card.addEventListener('keydown', (event) => {
      const order = KANBAN_COLUMNS.map((c) => c.id);
      const index = order.indexOf(task.stage || 'todo');
      let next = null;
      if (event.key === 'ArrowRight') next = order[Math.min(index + 1, order.length - 1)];
      else if (event.key === 'ArrowLeft') next = order[Math.max(index - 1, 0)];
      if (!next || next === task.stage) return;
      // Only intercept the keys on the card itself, never on inner controls.
      if (event.target !== card) return;
      event.preventDefault();

      api('POST', `/tasks/${task.id}/stage`, { stage: next })
        .then(() => {
          toast(`Moved to ${KANBAN_COLUMNS.find((c) => c.id === next)?.label}`, 'success');
          navigate('tasks');
        })
        .catch((err) => toast(err.message, 'error'));
    });
  });
}

/**
 * Create/edit dialog. The schedule is chosen from presets or a friendly
 * "every N" form; the generated cron stays an implementation detail.
 */
function showTaskForm(task, agents) {
  const isEdit = Boolean(task);
  const currentCron = task?.schedule_cron || '';
  const presetMatch = SCHEDULE_PRESETS.find((p) => p.cron === currentCron);
  const mode = !currentCron ? 'manual' : presetMatch ? presetMatch.label : 'custom';

  const presetOptions = SCHEDULE_PRESETS.map((p) => `
    <option value="${esc(p.cron)}" ${p.cron === currentCron ? 'selected' : ''}>
      ${esc(p.label)} — ${esc(p.hint)}
    </option>`).join('');

  const modal = openModal({
    title: isEdit ? 'Edit task' : 'New task',
    bodyHtml: `
      <div class="form-row">
        <label class="field-label" for="task-title">Name</label>
        <input id="task-title" placeholder="Morning inbox summary" value="${esc(task?.title || '')}">
      </div>
      <div class="form-row">
        <label class="field-label" for="task-objective">What should the agent do?</label>
        <textarea id="task-objective" placeholder="Summarize unread email and post the highlights to the team channel.">${esc(task?.objective || '')}</textarea>
      </div>
      <div class="form-row">
        <label class="field-label" for="task-agent">Agent</label>
        <select id="task-agent">
          ${agents.map((a) => `<option value="${esc(a.id)}" ${task?.agent_id === a.id ? 'selected' : ''}>${esc(a.name)}</option>`).join('')}
        </select>
      </div>
      <div class="form-row">
        <label class="field-label" for="task-schedule">When should it run?</label>
        <select id="task-schedule">
          <option value="" ${mode === 'manual' ? 'selected' : ''}>Only when I ask</option>
          ${presetOptions}
          <option value="__custom" ${mode === 'custom' ? 'selected' : ''}>Custom schedule…</option>
        </select>
        <div class="field-hint" id="task-schedule-hint"></div>
      </div>
      <div class="form-row hidden" id="task-custom-row">
        <label class="field-label" for="task-cron">Cron expression</label>
        <input id="task-cron" placeholder="0 9 * * 1-5" value="${esc(currentCron)}">
        <div class="field-hint">Five fields: minute hour day-of-month month day-of-week.</div>
      </div>
    `,
    footerHtml: `
      <button class="btn btn-secondary" data-close>Cancel</button>
      <button class="btn" id="task-save">${isEdit ? 'Save changes' : 'Create task'}</button>
    `,
  });

  const root = modal.root;
  const scheduleSelect = root.querySelector('#task-schedule');
  const hint = root.querySelector('#task-schedule-hint');
  const customRow = root.querySelector('#task-custom-row');
  const cronInput = root.querySelector('#task-cron');

  const syncScheduleUi = () => {
    const value = scheduleSelect.value;
    customRow.classList.toggle('hidden', value !== '__custom');
    if (value === '__custom') {
      hint.textContent = cronInput.value.trim()
        ? `Reads as: ${describeCron(cronInput.value.trim())}`
        : 'Describe the schedule as a cron expression.';
    } else if (!value) {
      hint.textContent = 'This task runs only when you press Run now.';
    } else {
      const preset = SCHEDULE_PRESETS.find((p) => p.cron === value);
      hint.textContent = preset ? `${preset.hint} (${describeCron(value)})` : '';
    }
  };
  scheduleSelect.addEventListener('change', syncScheduleUi);
  cronInput.addEventListener('input', syncScheduleUi);
  syncScheduleUi();

  root.querySelector('#task-save').addEventListener('click', async () => {
    const title = root.querySelector('#task-title').value.trim();
    const objective = root.querySelector('#task-objective').value.trim();
    const agentId = root.querySelector('#task-agent').value;
    const choice = scheduleSelect.value;
    const cron = choice === '__custom' ? cronInput.value.trim() : choice;

    if (!title || !objective) { toast('Name and objective are required', 'error'); return; }
    if (!agentId) { toast('Create an agent in Chat first', 'error'); return; }
    if (choice === '__custom' && !cron) { toast('Enter a cron expression or pick a preset', 'error'); return; }

    const body = { title, objective, agent_id: agentId };
    // `schedule: null` explicitly removes an existing schedule; omitting it
    // would leave the old cron in place.
    body.schedule = cron ? { cron } : null;

    const saveBtn = root.querySelector('#task-save');
    saveBtn.disabled = true;
    try {
      if (isEdit) {
        await api('PATCH', `/tasks/${task.id}`, body);
        toast('Task updated', 'success');
      } else {
        await api('POST', '/tasks', body);
        toast('Task created', 'success');
      }
      modal.close();
      navigate('tasks');
    } catch (err) {
      toast(err.message, 'error');
      saveBtn.disabled = false;
    }
  });
}

/// Execution history: each run with its status, duration, result and steps.
async function showRunHistory(task) {
  const modal = openModal({
    title: `History — ${task.title}`,
    bodyHtml: '<div class="muted">Loading runs…</div>',
  });
  const body = modal.root.querySelector('.modal-body');

  try {
    const { items: runs } = await api('GET', `/tasks/${task.id}/runs?limit=20`);
    if (!runs.length) {
      body.innerHTML = emptyState('This task has not run yet.', '⏱');
      return;
    }

    const rows = await Promise.all(runs.map(async (run) => {
      let stepsHtml = '<div class="muted" style="font-size:.85rem">No recorded steps</div>';
      try {
        const { items: steps } = await api('GET', `/task-runs/${run.id}/steps?limit=50`);
        if (steps.length) {
          stepsHtml = steps.map((s) => `
            <details class="run-step">
              <summary>
                <span>Step ${s.step_index}</span>
                <span class="badge ${esc(s.status)}">${esc(s.status)}</span>
              </summary>
              <div class="run-step-body">
                <div><strong>Input</strong></div>
                <pre>${esc(JSON.stringify(s.input, null, 2))}</pre>
                ${s.output ? `<div><strong>Output</strong></div><pre>${esc(JSON.stringify(s.output, null, 2))}</pre>` : ''}
                ${s.error_message ? `<div class="error">${esc(s.error_message)}</div>` : ''}
              </div>
            </details>`).join('');
        }
      } catch { /* steps unavailable */ }

      const duration = formatDuration(run.started_at, run.finished_at);
      return `
        <details class="run-entry">
          <summary>
            <span class="badge ${esc(run.status)}">${esc(run.status)}</span>
            <span class="muted" style="font-size:.83rem">
              ${esc(run.trigger)} · ${esc(formatDateTime(run.started_at || run.created_at))}
            </span>
            ${duration ? `<span class="run-time">${esc(duration)}</span>` : ''}
          </summary>
          <div class="run-step-body">
            ${run.result ? `<div><strong>Result</strong></div><pre>${esc(JSON.stringify(run.result, null, 2))}</pre>` : ''}
            ${run.error_message ? `<div class="error">${esc(run.error_message)}</div>` : ''}
            <div style="margin:10px 0 6px"><strong>Steps</strong></div>
            ${stepsHtml}
          </div>
        </details>`;
    }));

    body.innerHTML = rows.join('');
  } catch (err) {
    body.innerHTML = `<p class="error">Failed to load history: ${esc(err.message)}</p>`;
  }
}


// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

let notificationFilter = sessionStorage.getItem('auren_notif_filter') || 'all';

const NOTIFICATION_FILTERS = [
  { id: 'all', label: 'All' },
  { id: 'unread', label: 'Unread' },
  { id: 'action', label: 'Needs a decision' },
  { id: 'warning', label: 'Warnings' },
  { id: 'error', label: 'Errors' },
];

/** Icon and accent for a notification severity. */
function severityMeta(severity) {
  switch (severity) {
    case 'error': return { icon: '✕', cls: 'sev-error' };
    case 'warning': return { icon: '!', cls: 'sev-warning' };
    case 'success': return { icon: '✓', cls: 'sev-success' };
    default: return { icon: 'i', cls: 'sev-info' };
  }
}

views.notifications = async (content) => {
  const { items } = await api('GET', '/notifications?limit=100');

  const unread = items.filter((n) => !n.read_at).length;
  const pendingActions = items.filter((n) => n.actions?.length && !n.action_id).length;

  const visible = items.filter((n) => {
    switch (notificationFilter) {
      case 'unread': return !n.read_at;
      case 'action': return n.actions?.length > 0 && !n.action_id;
      case 'warning': return n.severity === 'warning';
      case 'error': return n.severity === 'error';
      default: return true;
    }
  });

  const counts = {
    all: items.length,
    unread,
    action: pendingActions,
    warning: items.filter((n) => n.severity === 'warning').length,
    error: items.filter((n) => n.severity === 'error').length,
  };

  content.innerHTML = `
    ${pageHead(
      'Notifications',
      'Updates from the agent and the system. Some carry actions you can take right away.',
      `
        <button class="btn btn-secondary" id="notif-read-all" ${unread ? '' : 'disabled'}>Mark all read</button>
        <button class="btn btn-secondary" id="notif-clear" ${items.length ? '' : 'disabled'}>Clear read</button>
      `,
    )}
    <div class="task-toolbar">
      ${NOTIFICATION_FILTERS.map((f) => `
        <button class="filter-chip ${f.id === notificationFilter ? 'active' : ''}" data-notif-filter="${f.id}">
          ${esc(f.label)}${counts[f.id] ? ` · ${counts[f.id]}` : ''}
        </button>`).join('')}
    </div>
    <div class="notif-list">
      ${visible.length ? visible.map((n) => notificationCardHtml(n)).join('')
        : emptyState(
            items.length ? 'Nothing in this filter.' : 'No notifications yet.',
            '✉',
          )}
    </div>
  `;

  content.querySelectorAll('[data-notif-filter]').forEach((chip) => {
    chip.addEventListener('click', () => {
      notificationFilter = chip.dataset.notifFilter;
      sessionStorage.setItem('auren_notif_filter', notificationFilter);
      navigate('notifications');
    });
  });

  const readAll = document.getElementById('notif-read-all');
  if (readAll) {
    readAll.addEventListener('click', async () => {
      try {
        await api('POST', '/notifications/read-all', {});
        refreshNotificationBadge();
        navigate('notifications');
      } catch (err) { toast(err.message, 'error'); }
    });
  }

  const clear = document.getElementById('notif-clear');
  if (clear) {
    clear.addEventListener('click', async () => {
      const read = items.filter((n) => n.read_at);
      if (!read.length) { toast('No read notifications to clear', 'error'); return; }
      if (!confirm(`Delete ${read.length} read notification(s)?`)) return;
      clear.disabled = true;
      let removed = 0;
      let failed = 0;
      for (const n of read) {
        try {
          await api('DELETE', `/notifications/${n.id}`);
          removed += 1;
        } catch { failed += 1; }
      }
      refreshNotificationBadge();
      if (failed) toast(`Cleared ${removed}, ${failed} failed`, 'error');
      else toast(`Cleared ${removed} notification${removed === 1 ? '' : 's'}`, 'success');
      navigate('notifications');
    });
  }

  // Per-notification interactions.
  content.querySelectorAll('.notif-card').forEach((card) => {
    const id = card.dataset.id;

    card.querySelectorAll('[data-notif-action]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const actionId = btn.dataset.notifAction;
        const kind = btn.dataset.kind;
        if (kind === 'agent' && !confirm('Let the agent act on this now?')) return;

        btn.disabled = true;
        card.querySelectorAll('[data-notif-action]').forEach((b) => { b.disabled = true; });
        try {
          const result = await api('POST', `/notifications/${id}/actions/${actionId}`, {});
          if (result.conversation_id && result.reply) {
            toast('The agent is on it — see Chat', 'success');
            currentConversationId = result.conversation_id;
            sessionStorage.setItem('auren_conv', currentConversationId);
          } else {
            toast('Done', 'success');
          }
          navigate('notifications');
        } catch (err) {
          toast(err.message, 'error');
          btn.disabled = false;
          card.querySelectorAll('[data-notif-action]').forEach((b) => { b.disabled = false; });
        }
      });
    });

    const readBtn = card.querySelector('[data-notif-read]');
    if (readBtn) {
      readBtn.addEventListener('click', async () => {
        try {
          await api('POST', `/notifications/${id}/read`, {});
          refreshNotificationBadge();
          navigate('notifications');
        } catch (err) { toast(err.message, 'error'); }
      });
    }

    const deleteBtn = card.querySelector('[data-notif-delete]');
    if (deleteBtn) {
      deleteBtn.addEventListener('click', async () => {
        try {
          await api('DELETE', `/notifications/${id}`);
          refreshNotificationBadge();
          navigate('notifications');
        } catch (err) { toast(err.message, 'error'); }
      });
    }
  });
};

/** One notification card with its severity, actions, and metadata. */
function notificationCardHtml(n) {
  const sev = severityMeta(n.severity);
  const actions = Array.isArray(n.actions) ? n.actions : [];
  const acted = Boolean(n.acted_at);

  const actionButtons = actions.map((a) => {
    const style = a.style === 'primary' ? '' : a.style === 'danger' ? ' btn-danger' : ' btn-secondary';
    return `<button class="btn btn-sm${style}" data-notif-action="${esc(a.id)}" data-kind="${esc(a.kind)}" ${acted ? 'disabled' : ''}>${esc(a.label)}</button>`;
  }).join('');

  return `
    <div class="notif-card ${n.read_at ? 'is-read' : 'is-unread'} ${sev.cls}" data-id="${esc(n.id)}">
      <div class="notif-head">
        <span class="notif-sev ${sev.cls}" aria-hidden="true">${sev.icon}</span>
        <div class="notif-title-wrap">
          <div class="notif-title">${esc(n.title)}</div>
          <div class="notif-meta muted">
            ${esc(relativeTime(n.created_at))}
            ${n.notification_type ? ` · ${esc(n.notification_type.replace(/_/g, ' '))}` : ''}
            ${acted ? ' · action taken' : ''}
          </div>
        </div>
        <span class="spacer"></span>
        ${n.read_at ? '' : '<span class="badge draft plain">unread</span>'}
        <button class="btn-icon" data-notif-read="${esc(n.id)}" title="Mark as read"
          ${n.read_at ? 'disabled' : ''}>✓</button>
        <button class="btn-icon danger" data-notif-delete="${esc(n.id)}" title="Delete">✕</button>
      </div>
      ${n.body ? `<div class="notif-body">${esc(n.body)}</div>` : ''}
      ${actionButtons ? `<div class="notif-actions">${actionButtons}</div>` : ''}
    </div>`;
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
    ${pageHead('Skills', 'Capabilities the agent can use. Install built-in packages or upload your own.')}
    ${availableBuiltins.length ? `
      <h3 class="section-title">Available built-in skills</h3>
      <div class="grid" style="margin-bottom:24px">${availableBuiltins.map((b) => `
        <div class="card interactive" data-builtin="${esc(b.id)}">
          <h3>${esc(b.id.replace('-skill', '').replace(/(^|\b)(\w)/g, (m) => m.toUpperCase()))}</h3>
          <p class="muted" style="margin-bottom:14px; font-size:.87rem">${esc(b.description)}</p>
          <button class="btn btn-sm" data-install="${esc(b.id)}">Install</button>
        </div>`).join('')}</div>` : ''}
    <div class="card" style="margin-bottom:24px">
      <h3>Import from a GitHub repository</h3>
      <p class="muted" style="margin:6px 0 14px; font-size:.87rem">
        Point at a repository to list the skills, tools, and MCP servers it provides. Nothing is installed until you choose it.
      </p>
      <div class="form-row" style="max-width:560px">
        <div style="display:flex;gap:8px">
          <input id="import-repo" placeholder="owner/repo or https://github.com/owner/repo" style="flex:1">
          <button class="btn" id="import-discover">Scan</button>
        </div>
        <p class="field-hint">Private repositories use your connected GitHub account.</p>
      </div>
      <div id="import-results"></div>
    </div>
    <div class="card" style="margin-bottom:24px">
      <h3>Install a custom skill package</h3>
      <p class="muted" style="margin:6px 0 14px; font-size:.87rem">Upload a skill ZIP. It is validated before installation.</p>
      <label class="file-drop" id="skill-drop" for="skill-file">
        <input type="file" id="skill-file" accept=".zip" hidden>
        <span id="skill-drop-text">📁 Choose a .zip file or drop it here</span>
      </label>
      <button class="btn hidden" id="skill-upload" style="margin-top:14px">Upload and install</button>
    </div>
    <h3 class="section-title">Installed skills</h3>
    ${installed.items.length ? `<div class="grid">${installed.items.map((s) => `
      <div class="card" data-id="${esc(s.id)}">
        <div style="display:flex;align-items:center;gap:8px;margin-bottom:6px">
          <h3 style="margin:0">${esc(s.name)}</h3>
          <span class="spacer"></span>
          <span class="badge ${esc(s.status)}">${esc(s.status)}</span>
        </div>
        <p class="muted" style="margin-bottom:14px; font-size:.82rem">${esc(s.id)}</p>
        <div class="task-actions">
          <button class="btn btn-sm btn-secondary" data-action="enable">Enable</button>
          <button class="btn btn-sm btn-secondary" data-action="disable">Disable</button>
          <button class="btn btn-sm btn-secondary" data-action="versions">Versions</button>
          <button class="btn btn-sm btn-danger" data-action="uninstall">Uninstall</button>
        </div>
        <div class="skill-versions" style="margin-top:10px"></div>
      </div>`).join('')}</div>` : emptyState('No skills installed yet.', '◆')}
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

  wireGithubImport(content);

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

/// Wire the "Import from GitHub" card: scan a repository, then install a
/// discovered skill package through the normal validation pipeline.
function wireGithubImport(content) {
  const input = content.querySelector('#import-repo');
  const scanBtn = content.querySelector('#import-discover');
  const results = content.querySelector('#import-results');
  if (!input || !scanBtn || !results) return;

  // Remember the last scanned repository so the user can retry.
  const lastRepo = sessionStorage.getItem('auren_import_repo');
  if (lastRepo) input.value = lastRepo;

  const runScan = async () => {
    const repository = input.value.trim();
    if (!repository) { toast('Enter a repository', 'error'); return; }
    sessionStorage.setItem('auren_import_repo', repository);
    scanBtn.disabled = true;
    scanBtn.textContent = 'Scanning…';
    results.innerHTML = '<div class="skeleton" style="height:64px"></div>';
    try {
      const found = await api('POST', '/skills/import/discover', { repository });
      results.innerHTML = importResultsHtml(found);
      wireInstallButtons(found);
    } catch (err) {
      results.innerHTML = `<p class="error">${esc(err.message)}</p>`;
    } finally {
      scanBtn.disabled = false;
      scanBtn.textContent = 'Scan';
    }
  };

  scanBtn.addEventListener('click', runScan);
  input.addEventListener('keydown', (event) => { if (event.key === 'Enter') runScan(); });

  const wireInstallButtons = (found) => {
    results.querySelectorAll('[data-import-install]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        btn.disabled = true;
        btn.textContent = 'Installing…';
        try {
          const { upload_id: uploadId } = await api('POST', '/skills/import/install', {
            repository: found.repository,
            reference: found.reference,
            skill_path: btn.dataset.importInstall,
            install: true,
          });
          toast('Package downloaded; validating…', 'success');
          // Same progress protocol as any upload.
          const poll = setInterval(async () => {
            try {
              const status = await api('GET', `/skills/uploads/${uploadId}`);
              if (['installed', 'failed', 'cancelled'].includes(status.status)) {
                clearInterval(poll);
                if (status.status === 'installed') toast(`${status.skill_id} installed`, 'success');
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

    results.querySelectorAll('[data-import-mcp]').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const name = btn.dataset.importMcpName;
        const url = btn.dataset.importMcpUrl;
        if (!url) { toast('This server needs a local process and cannot be added here', 'error'); return; }
        btn.disabled = true;
        try {
          await api('POST', '/mcp-servers', { name, url, auth_header: null });
          toast('MCP server connected', 'success');
          navigate('settings');
        } catch (err) {
          toast(err.message, 'error');
          btn.disabled = false;
        }
      });
    });
  };

  // Re-scan automatically when returning to the view with a saved repo.
  if (lastRepo && results) runScan();
}

/** Render a discovery response: skills, tools, and MCP servers found. */
function importResultsHtml(found) {
  const total = found.skills.length + found.tools.length + found.mcp_servers.length;
  const notes = (found.notes || []).length
    ? `<div class="import-notes">${found.notes.map((n) => `<div class="muted">${esc(n)}</div>`).join('')}</div>`
    : '';

  if (!total) {
    return `
      <div class="empty-state" style="margin-top:14px">
        <div class="empty-icon">🔍</div>
        <div>Nothing importable found in ${esc(found.repository)} (${esc(found.reference)}).</div>
        <div class="muted" style="font-size:.83rem">Expected a <code>skill.json</code>, a <code>tools/</code> folder, or a <code>.mcp.json</code> descriptor.</div>
      </div>
      ${notes}`;
  }

  const skills = found.skills.length ? `
    <div class="import-group">
      <div class="import-group-title">Skills · ${found.skills.length}</div>
      ${found.skills.map((s) => `
        <div class="import-item">
          <div style="min-width:0">
            <strong>${esc(s.name)}</strong>
            <span class="badge active plain" style="margin-left:6px">v${esc(s.version)}</span>
            <div class="muted" style="font-size:.82rem">${esc(s.description || s.id)}</div>
            <div class="muted" style="font-size:.76rem"><code>${esc(s.path)}</code></div>
          </div>
          <span class="spacer"></span>
          <button class="btn btn-sm" data-import-install="${esc(s.path)}">Install</button>
        </div>`).join('')}
    </div>` : '';

  const tools = found.tools.length ? `
    <div class="import-group">
      <div class="import-group-title">Tool definitions · ${found.tools.length}</div>
      ${found.tools.map((t) => `
        <div class="import-item">
          <div style="min-width:0">
            <code>${esc(t.name)}</code>
            <div class="muted" style="font-size:.82rem">${esc(t.description || 'No description')}</div>
            <div class="muted" style="font-size:.76rem">${esc(t.path)}</div>
          </div>
          <span class="spacer"></span>
          <span class="badge disabled plain">bundle with a skill</span>
        </div>`).join('')}
    </div>` : '';

  const servers = found.mcp_servers.length ? `
    <div class="import-group">
      <div class="import-group-title">MCP servers · ${found.mcp_servers.length}</div>
      ${found.mcp_servers.map((m) => `
        <div class="import-item">
          <div style="min-width:0">
            <strong>${esc(m.name)}</strong>
            <span class="badge ${m.transport === 'http' ? 'active' : 'disabled'} plain" style="margin-left:6px">${esc(m.transport)}</span>
            <div class="muted" style="font-size:.82rem">${esc(m.url || 'Runs as a local process')}</div>
          </div>
          <span class="spacer"></span>
          <button class="btn btn-sm btn-secondary" data-import-mcp
            data-import-mcp-name="${esc(m.name)}" data-import-mcp-url="${esc(m.url || '')}"
            ${m.transport === 'http' ? '' : 'disabled title="stdio servers cannot be launched server-side"'}>
            ${m.transport === 'http' ? 'Connect' : 'Unsupported'}
          </button>
        </div>`).join('')}
    </div>` : '';

  return `
    <div class="import-summary muted">
      ${esc(found.repository)} @ <code>${esc(found.reference)}</code> — ${total} item${total === 1 ? '' : 's'} found
    </div>
    ${skills}${tools}${servers}${notes}`;
}

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
    ${pageHead('Integrations', 'Accounts the agent can act on: mail, calendars, repositories and more.')}
    <div class="card" style="margin-bottom:20px">
      <h3>Connect a provider</h3>
      <div class="form-row" style="margin-top:14px; max-width:440px">
        <label class="field-label" for="integration-type">Provider</label>
        <select id="integration-type">
          ${catalog.items.map((c) => `<option value="${esc(c.provider_type)}">${esc(c.provider_type)}</option>`).join('')}
        </select>
        <label class="field-label" for="integration-name">Display name</label>
        <input id="integration-name" placeholder="Work Gmail">
        <label class="field-label" for="integration-token">Access token (optional)</label>
        <input id="integration-token" type="password" placeholder="Leave empty to use OAuth when available">
        <button class="btn" id="integration-connect">Connect</button>
        <p class="field-hint">Google providers use OAuth when GOOGLE_CLIENT_ID is configured; otherwise connect manually with an access token.</p>
      </div>
    </div>
    ${connections.items.length ? `<table class="table">
      <thead><tr><th>Provider</th><th>Name</th><th>Status</th><th></th></tr></thead>
      <tbody>${connections.items.map((i) => `
        <tr data-id="${esc(i.id)}">
          <td>${esc(i.provider_type)}</td><td>${esc(i.display_name)}</td>
          <td><span class="badge ${esc(i.status)}">${esc(i.status)}</span></td>
          <td style="text-align:right"><button class="btn btn-sm btn-danger" data-action="disconnect">Disconnect</button></td>
        </tr>`).join('')}
      </tbody></table>` : emptyState('No integrations connected.', '⇄')}
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
    ${pageHead('Approvals', 'Operations the agent wants to perform. Approve or reject each one.',
      items.length ? `<span class="badge pending">${items.length} waiting</span>` : '')}
    ${items.length ? items.map((a) => `
      <div class="card" style="margin-bottom:12px" data-id="${esc(a.id)}">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:10px; flex-wrap:wrap">
          <span class="badge ${esc(a.status)}">${esc(a.status)}</span>
          ${a.tool_name ? `<code class="mono" style="font-size:.83rem">${esc(a.tool_name)}</code>` : ''}
          <span class="spacer"></span>
          <span class="muted" style="font-size:.8rem">expires ${esc(relativeTime(a.expires_at))}</span>
        </div>
        ${a.args ? `<pre class="tool-trace-params" style="margin:0 0 14px">${esc(JSON.stringify(a.args, null, 2))}</pre>` : ''}
        <div style="display:flex; gap:8px">
          <button class="btn" data-action="approve">Approve</button>
          <button class="btn btn-danger" data-action="reject">Reject</button>
        </div>
      </div>`).join('') : emptyState('Nothing to approve right now.', '✓')}
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
    ${pageHead('Settings', 'Agents, execution policy, model providers and tool servers.')}

    <h3 class="section-title" style="margin-top:0">Agents</h3>
    <p class="muted" style="margin-bottom:14px; font-size:.87rem">Agents are managed by the system: the assistant routes your requests automatically. You only need to touch them here to change instructions or remove one.</p>
    ${agents.items.map((a) => `
      <div class="card" style="margin-bottom:10px" data-agent="${esc(a.id)}">
        <div style="display:flex; align-items:center; gap:10px; flex-wrap:wrap">
          <strong>${esc(a.name)}</strong>
          <span class="badge ${esc(a.status)}">${esc(a.status)}</span>
          <span class="spacer"></span>
          <button class="btn btn-sm btn-secondary" data-action="edit-agent">Edit instructions</button>
          <button class="btn btn-sm btn-danger" data-action="delete-agent">Remove</button>
        </div>
        <div class="muted" style="font-size:.85rem; margin-top:8px; white-space:pre-wrap">${esc(a.instructions || 'No instructions')}</div>
      </div>`).join('')}

    <h3 class="section-title">Executions</h3>
    <div class="card" style="margin-bottom:20px">
      <label style="display:flex; align-items:center; gap:10px; cursor:pointer">
        <input type="checkbox" id="auto-approve-global" ${autoApprove.enabled ? 'checked' : ''}>
        <span>Run all tool executions without asking for approval</span>
      </label>
      <p class="muted" style="margin:8px 0 0; font-size:.85rem">When enabled, the agent executes tools immediately. Critical-risk operations still always ask. You can also override this per chat in the chat toolbar.</p>
    </div>

    <h3 class="section-title">LLM providers</h3>
    <div class="card" style="margin-bottom:16px">
      <div class="form-row" style="max-width:440px">
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
    <h3 class="section-title">MCP servers</h3>
    <div class="card" style="margin-bottom:16px">
      <div class="form-row" style="max-width:540px">
        <input id="mcp-name" placeholder="Server name">
        <input id="mcp-url" placeholder="MCP endpoint URL (https://…/mcp)">
        <input id="mcp-auth" type="password" placeholder="Authorization header value (optional)">
        <button class="btn" id="mcp-add">Connect MCP server</button>
      </div>
    </div>
    <div id="mcp-list"></div>
    <h3 class="section-title">Workspace</h3>
    <div class="card"><pre class="mono" style="font-size:.8rem;white-space:pre-wrap">${esc(JSON.stringify(workspaceConfig, null, 2))}</pre></div>
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
  current.querySelector('.chat-list-meta').textContent = 'just now';
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