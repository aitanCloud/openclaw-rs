// OpenClaw Orchestrator Frontend
// Placeholder — replace with esbuild output from: npx esbuild src/ts/app.ts --bundle --outfile=src/static/app.js --format=esm --target=es2022 --minify
//
// This file is embedded in the Rust binary via include_str!().
// The real TypeScript source lives in src/ts/.

// ── Token storage ────────────────────────────────────────────────

const TOKEN_KEY = 'openclaw_token';

function getToken() {
    return localStorage.getItem(TOKEN_KEY);
}

function setToken(token) {
    localStorage.setItem(TOKEN_KEY, token);
}

function clearToken() {
    localStorage.removeItem(TOKEN_KEY);
}

// ── DOM helpers ──────────────────────────────────────────────────

function h(tag, className, textContent) {
    const e = document.createElement(tag);
    if (className) e.className = className;
    if (textContent !== undefined) e.textContent = textContent;
    return e;
}

function clearChildren(element) {
    while (element.firstChild) element.removeChild(element.firstChild);
}

function stateBadge(state) {
    const colors = {
        provisioning: 'blue', running: 'green', blocked: 'red',
        completed: 'gray', failed: 'red', cancelled: 'yellow',
        planning: 'blue', plan_review: 'purple', approved: 'green',
        executing: 'green', merging: 'blue',
        pending: 'gray', scheduled: 'blue', skipped: 'yellow',
    };
    const color = colors[state] || 'gray';
    const badge = h('span', `badge badge--${color}`);
    badge.textContent = state.replace(/_/g, ' ');
    return badge;
}

function timeAgo(iso) {
    const diff = Date.now() - new Date(iso).getTime();
    if (isNaN(diff)) return iso;
    if (diff < 60000) return 'just now';
    if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`;
    if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`;
    return `${Math.floor(diff / 86400000)}d ago`;
}

function formatDate(iso) {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
        year: 'numeric', month: 'short', day: 'numeric',
        hour: '2-digit', minute: '2-digit', second: '2-digit',
    });
}

function formatCents(cents) {
    return `$${(cents / 100).toFixed(2)}`;
}

function shortId(uuid) {
    return uuid.slice(0, 8);
}

function truncate(s, maxLen) {
    if (s.length <= maxLen) return s;
    return s.slice(0, maxLen - 1) + '\u2026';
}

// ── API client ───────────────────────────────────────────────────

async function apiRequest(method, path, body) {
    const headers = { 'Content-Type': 'application/json' };
    const token = getToken();
    if (token) headers['Authorization'] = `Bearer ${token}`;

    const opts = { method, headers };
    if (body !== undefined) opts.body = JSON.stringify(body);

    const response = await fetch(path, opts);
    if (!response.ok) {
        if (response.status === 401) {
            clearToken();
            window.dispatchEvent(new CustomEvent('openclaw:auth-required'));
        }
        if (response.status === 503) {
            window.dispatchEvent(new CustomEvent('openclaw:maintenance'));
        }
        const text = await response.text().catch(() => '');
        throw new Error(`API ${response.status}: ${response.statusText} ${text}`);
    }
    return response.json();
}

const api = {
    listInstances: (cursor, limit) => {
        const p = new URLSearchParams();
        if (cursor) p.set('cursor', cursor);
        if (limit) p.set('limit', String(limit));
        const qs = p.toString();
        return apiRequest('GET', `/api/v1/instances${qs ? '?' + qs : ''}`);
    },
    getInstance: (id) => apiRequest('GET', `/api/v1/instances/${id}`),
    createInstance: (body) => apiRequest('POST', '/api/v1/instances', body),
    listCycles: (instanceId, cursor, limit) => {
        const p = new URLSearchParams();
        if (cursor) p.set('cursor', cursor);
        if (limit) p.set('limit', String(limit));
        const qs = p.toString();
        return apiRequest('GET', `/api/v1/instances/${instanceId}/cycles${qs ? '?' + qs : ''}`);
    },
    getCycle: (instanceId, cycleId) =>
        apiRequest('GET', `/api/v1/instances/${instanceId}/cycles/${cycleId}`),
    createCycle: (instanceId, body) =>
        apiRequest('POST', `/api/v1/instances/${instanceId}/cycles`, body),
    approvePlan: (instanceId, cycleId, body) =>
        apiRequest('POST', `/api/v1/instances/${instanceId}/cycles/${cycleId}/approve`, body),
    triggerMerge: (instanceId, cycleId, body) =>
        apiRequest('POST', `/api/v1/instances/${instanceId}/cycles/${cycleId}/merge`, body),
    listTasks: (instanceId, cycleId, cursor, limit) => {
        const p = new URLSearchParams();
        if (cycleId) p.set('cycle_id', cycleId);
        if (cursor) p.set('cursor', cursor);
        if (limit) p.set('limit', String(limit));
        const qs = p.toString();
        return apiRequest('GET', `/api/v1/instances/${instanceId}/tasks${qs ? '?' + qs : ''}`);
    },
    getRun: (instanceId, runId) =>
        apiRequest('GET', `/api/v1/instances/${instanceId}/runs/${runId}`),
    listBudgets: (instanceId, cursor, limit) => {
        const p = new URLSearchParams();
        if (cursor) p.set('cursor', cursor);
        if (limit) p.set('limit', String(limit));
        const qs = p.toString();
        return apiRequest('GET', `/api/v1/instances/${instanceId}/budgets${qs ? '?' + qs : ''}`);
    },
    listEvents: (instanceId, eventType, cursor, limit) => {
        const p = new URLSearchParams();
        if (eventType) p.set('event_type', eventType);
        if (cursor) p.set('cursor', cursor);
        if (limit) p.set('limit', String(limit));
        const qs = p.toString();
        return apiRequest('GET', `/api/v1/instances/${instanceId}/events${qs ? '?' + qs : ''}`);
    },
};

// ── Store ────────────────────────────────────────────────────────

class Store {
    constructor() {
        this.state = {
            instances: new Map(),
            instanceList: [],
            selectedInstanceId: null,
            selectedCycleId: null,
            wsConnected: false,
            lastEventSeq: new Map(),
            maintenanceMode: false,
            loading: false,
            error: null,
        };
        this.listeners = new Set();
    }

    getState() { return this.state; }

    subscribe(listener) {
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }

    notify() {
        for (const l of this.listeners) {
            try { l(this.state); } catch (e) { console.error('[store] listener error:', e); }
        }
    }

    update(partial) {
        this.state = { ...this.state, ...partial };
        this.notify();
    }

    setLoading(v) { this.update({ loading: v }); }
    setError(v) { this.update({ error: v }); }
    setWsConnected(v) { this.update({ wsConnected: v }); }
    setMaintenanceMode(v) { this.update({ maintenanceMode: v }); }
    setSelectedInstance(v) { this.update({ selectedInstanceId: v }); }
    setSelectedCycle(v) { this.update({ selectedCycleId: v }); }

    setLastEventSeq(instanceId, seq) {
        const m = new Map(this.state.lastEventSeq);
        m.set(instanceId, seq);
        this.update({ lastEventSeq: m });
    }

    setInstanceList(instances) { this.update({ instanceList: instances }); }

    setInstanceViewModel(id, vm) {
        const m = new Map(this.state.instances);
        m.set(id, vm);
        this.update({ instances: m });
    }

    async fetchInstances() {
        this.setLoading(true);
        this.setError(null);
        try {
            const resp = await api.listInstances();
            this.setInstanceList(resp.data);
        } catch (e) {
            this.setError(e.message || String(e));
        } finally {
            this.setLoading(false);
        }
    }

    async fetchInstanceDetail(instanceId) {
        this.setLoading(true);
        this.setError(null);
        try {
            const [instance, cyclesResp, tasksResp, budgetsResp] = await Promise.all([
                api.getInstance(instanceId),
                api.listCycles(instanceId),
                api.listTasks(instanceId),
                api.listBudgets(instanceId),
            ]);
            const totalSpentCents = budgetsResp.data.reduce(
                (sum, entry) => sum + Math.abs(entry.amount_cents), 0
            );
            this.setInstanceViewModel(instanceId, {
                instance, cycles: cyclesResp.data, tasks: tasksResp.data,
                budgetEntries: budgetsResp.data, totalSpentCents,
            });
        } catch (e) {
            this.setError(e.message || String(e));
        } finally {
            this.setLoading(false);
        }
    }

    async invalidate(instanceId) {
        if (this.state.selectedInstanceId === instanceId) {
            await this.fetchInstanceDetail(instanceId);
        }
        await this.fetchInstances();
    }
}

const store = new Store();

// ── WebSocket Manager ────────────────────────────────────────────

class WsManager {
    constructor(instanceId) {
        this.instanceId = instanceId;
        this.ws = null;
        this.reconnectAttempt = 0;
        this.reconnectTimer = null;
        this.destroyed = false;
    }

    connect() {
        if (this.destroyed || this.ws) return;
        const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        const url = `${proto}//${location.host}/api/v1/instances/${this.instanceId}/events/ws`;
        this.ws = new WebSocket(url);
        this.ws.onopen = () => this._onOpen();
        this.ws.onmessage = (e) => this._onMessage(e);
        this.ws.onclose = () => this._onClose();
        this.ws.onerror = () => {};
    }

    destroy() {
        this.destroyed = true;
        if (this.reconnectTimer !== null) { clearTimeout(this.reconnectTimer); this.reconnectTimer = null; }
        if (this.ws) { this.ws.onopen = null; this.ws.onmessage = null; this.ws.onclose = null; this.ws.onerror = null; this.ws.close(); this.ws = null; }
        store.setWsConnected(false);
    }

    _onOpen() {
        const token = getToken();
        if (!token) { this.ws.close(); return; }
        this.ws.send(JSON.stringify({ type: 'auth', token }));
        const sinceSeq = store.getState().lastEventSeq.get(this.instanceId) || 0;
        this.ws.send(JSON.stringify({ type: 'subscribe', since_seq: sinceSeq }));
        this.reconnectAttempt = 0;
        store.setWsConnected(true);
    }

    _onMessage(event) {
        let msg;
        try { msg = JSON.parse(event.data); } catch { return; }
        switch (msg.type) {
            case 'event':
                store.setLastEventSeq(this.instanceId, msg.seq);
                store.invalidate(this.instanceId);
                break;
            case 'backfill_complete':
                store.setLastEventSeq(this.instanceId, msg.last_sent_seq);
                store.invalidate(this.instanceId);
                break;
            case 'heartbeat': break;
            case 'reconnect': this.ws.close(); break;
            case 'error': console.error('[ws] server error:', msg.message); break;
        }
    }

    _onClose() {
        this.ws = null;
        store.setWsConnected(false);
        if (this.destroyed) return;
        const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempt), 30000);
        this.reconnectAttempt++;
        this.reconnectTimer = setTimeout(() => { this.reconnectTimer = null; this.connect(); }, delay);
    }
}

// ── Components ───────────────────────────────────────────────────

// Instance List
class InstanceList {
    constructor(container) {
        this.el = h('div', 'instance-list');
        container.appendChild(this.el);
        this.renderKey = '';
        this.el.addEventListener('click', (e) => {
            const row = e.target.closest('[data-instance-id]');
            if (row) location.hash = `#/instances/${row.dataset.instanceId}`;
        });
    }

    update(state) {
        const key = JSON.stringify(state.instanceList.map(i => `${i.id}:${i.state}:${i.last_heartbeat}`));
        if (key === this.renderKey) return;
        this.renderKey = key;
        clearChildren(this.el);

        const header = h('div', 'section-header');
        header.appendChild(h('h2', 'section-title', 'Instances'));
        header.appendChild(h('span', 'section-count', `${state.instanceList.length}`));
        this.el.appendChild(header);

        if (state.loading && state.instanceList.length === 0) {
            this.el.appendChild(h('div', 'empty-state', 'Loading instances...'));
            return;
        }
        if (state.instanceList.length === 0) {
            this.el.appendChild(h('div', 'empty-state', 'No instances found.'));
            return;
        }

        const table = h('table', 'data-table');
        const thead = h('thead');
        const hr = h('tr');
        ['Name', 'State', 'Project', 'Last Heartbeat'].forEach(c => hr.appendChild(h('th', '', c)));
        thead.appendChild(hr);
        table.appendChild(thead);

        const tbody = h('tbody');
        for (const inst of state.instanceList) {
            const row = h('tr', 'data-table__row clickable');
            row.dataset.instanceId = inst.id;

            const nameCell = h('td', 'data-table__cell');
            nameCell.appendChild(h('span', 'instance-name', truncate(inst.name, 40)));
            nameCell.appendChild(h('span', 'instance-id', inst.id.slice(0, 8)));
            row.appendChild(nameCell);

            const stateCell = h('td', 'data-table__cell');
            stateCell.appendChild(stateBadge(inst.state));
            if (inst.block_reason) stateCell.appendChild(h('span', 'block-reason', inst.block_reason));
            row.appendChild(stateCell);

            row.appendChild(h('td', 'data-table__cell mono', inst.project_id.slice(0, 8)));

            const hbCell = h('td', 'data-table__cell text-secondary', timeAgo(inst.last_heartbeat));
            hbCell.title = inst.last_heartbeat;
            row.appendChild(hbCell);

            tbody.appendChild(row);
        }
        table.appendChild(tbody);
        this.el.appendChild(table);
    }

    destroy() { this.el.remove(); }
}

// Instance Detail
class InstanceDetail {
    constructor(container) {
        this.el = h('div', 'instance-detail');
        container.appendChild(this.el);
        this.renderKey = '';
        this.el.addEventListener('click', (e) => {
            const row = e.target.closest('[data-cycle-id]');
            if (row) location.hash = `#/instances/${row.dataset.instanceId}/cycles/${row.dataset.cycleId}`;
        });
    }

    update(state) {
        const instanceId = state.selectedInstanceId;
        if (!instanceId) return;
        const vm = state.instances.get(instanceId);
        if (!vm) {
            const k = `loading:${instanceId}`;
            if (k === this.renderKey) return;
            this.renderKey = k;
            clearChildren(this.el);
            this.el.appendChild(h('div', 'empty-state', 'Loading instance...'));
            return;
        }

        const key = JSON.stringify({
            id: vm.instance.id, state: vm.instance.state,
            cycles: vm.cycles.map(c => `${c.id}:${c.state}`),
            budget: vm.totalSpentCents, tasks: vm.tasks.length,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);

        // Header
        const header = h('div', 'detail-header');
        const back = h('a', 'back-link', 'All Instances');
        back.href = '#/';
        header.appendChild(back);

        const titleRow = h('div', 'detail-title-row');
        titleRow.appendChild(h('h2', 'detail-title', vm.instance.name));
        titleRow.appendChild(stateBadge(vm.instance.state));
        header.appendChild(titleRow);

        const meta = h('div', 'detail-meta');
        meta.appendChild(h('span', 'meta-item', `ID: ${shortId(vm.instance.id)}`));
        meta.appendChild(h('span', 'meta-item', `Project: ${shortId(vm.instance.project_id)}`));
        meta.appendChild(h('span', 'meta-item', `Started: ${formatDate(vm.instance.started_at)}`));
        meta.appendChild(h('span', 'meta-item', `Heartbeat: ${timeAgo(vm.instance.last_heartbeat)}`));
        header.appendChild(meta);

        if (vm.instance.block_reason) {
            const alert = h('div', 'alert alert--warning');
            alert.appendChild(h('strong', '', 'Blocked: '));
            alert.appendChild(document.createTextNode(vm.instance.block_reason));
            header.appendChild(alert);
        }
        this.el.appendChild(header);

        // Summary cards
        const cards = h('div', 'summary-cards');
        cards.appendChild(this._card('Cycles', String(vm.cycles.length), this._breakdown(vm.cycles, 'state')));
        cards.appendChild(this._card('Tasks', String(vm.tasks.length), this._breakdown(vm.tasks, 'state')));
        cards.appendChild(this._card('Budget Spent', formatCents(vm.totalSpentCents), `${vm.budgetEntries.length} ledger entries`));
        this.el.appendChild(cards);

        // Cycles table
        const section = h('div', 'section');
        const sh = h('div', 'section-header');
        sh.appendChild(h('h3', 'section-title', 'Cycles'));
        section.appendChild(sh);

        if (vm.cycles.length === 0) {
            section.appendChild(h('div', 'empty-state', 'No cycles yet.'));
        } else {
            const table = h('table', 'data-table');
            const thead = h('thead');
            const hr = h('tr');
            ['ID', 'State', 'Prompt', 'Created', 'Updated'].forEach(c => hr.appendChild(h('th', '', c)));
            thead.appendChild(hr);
            table.appendChild(thead);

            const tbody = h('tbody');
            for (const cycle of vm.cycles) {
                const row = h('tr', 'data-table__row clickable');
                row.dataset.cycleId = cycle.id;
                row.dataset.instanceId = vm.instance.id;
                row.appendChild(h('td', 'data-table__cell mono', shortId(cycle.id)));
                const sc = h('td', 'data-table__cell');
                sc.appendChild(stateBadge(cycle.state));
                row.appendChild(sc);
                row.appendChild(h('td', 'data-table__cell', truncate(cycle.prompt, 60)));
                row.appendChild(h('td', 'data-table__cell text-secondary', timeAgo(cycle.created_at)));
                row.appendChild(h('td', 'data-table__cell text-secondary', timeAgo(cycle.updated_at)));
                tbody.appendChild(row);
            }
            table.appendChild(tbody);
            section.appendChild(table);
        }
        this.el.appendChild(section);
    }

    destroy() { this.el.remove(); }

    _card(title, value, subtitle) {
        const card = h('div', 'summary-card');
        card.appendChild(h('div', 'summary-card__title', title));
        if (value) card.appendChild(h('div', 'summary-card__value', value));
        if (subtitle) card.appendChild(h('div', 'summary-card__subtitle', subtitle));
        return card;
    }

    _breakdown(items, key) {
        const m = new Map();
        for (const i of items) { const v = i[key]; m.set(v, (m.get(v) || 0) + 1); }
        return [...m.entries()].map(([s, n]) => `${n} ${s}`).join(', ');
    }
}

// Cycle Detail
class CycleDetail {
    constructor(container, instanceId, cycleId) {
        this.el = h('div', 'cycle-detail');
        container.appendChild(this.el);
        this.instanceId = instanceId;
        this.cycleId = cycleId;
        this.renderKey = '';
        this.cycle = null;
        this.cycleTasks = [];
        this.loading = true;
        this._fetchData();
    }

    async _fetchData() {
        try {
            const [cycle, tasksResp] = await Promise.all([
                api.getCycle(this.instanceId, this.cycleId),
                api.listTasks(this.instanceId, this.cycleId),
            ]);
            this.cycle = cycle;
            this.cycleTasks = tasksResp.data;
        } catch (e) { console.error('[cycle-detail]', e); }
        this.loading = false;
        this._render();
    }

    update(state) {
        const vm = state.instances.get(this.instanceId);
        if (vm) {
            const c = vm.cycles.find(c => c.id === this.cycleId);
            if (c) {
                this.cycle = c;
                this.cycleTasks = vm.tasks.filter(t => t.cycle_id === this.cycleId);
            }
        }
        this._render();
    }

    destroy() { this.el.remove(); }

    _render() {
        const key = JSON.stringify({
            cycle: this.cycle ? `${this.cycle.id}:${this.cycle.state}` : null,
            tasks: this.cycleTasks.map(t => `${t.id}:${t.state}`),
            loading: this.loading,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;
        clearChildren(this.el);

        if (this.loading && !this.cycle) {
            this.el.appendChild(h('div', 'empty-state', 'Loading cycle...'));
            return;
        }
        if (!this.cycle) {
            this.el.appendChild(h('div', 'empty-state', 'Cycle not found.'));
            return;
        }

        const cycle = this.cycle;

        // Header
        const header = h('div', 'detail-header');
        const back = h('a', 'back-link', 'Back to Instance');
        back.href = `#/instances/${this.instanceId}`;
        header.appendChild(back);

        const titleRow = h('div', 'detail-title-row');
        titleRow.appendChild(h('h2', 'detail-title', `Cycle ${shortId(cycle.id)}`));
        titleRow.appendChild(stateBadge(cycle.state));
        header.appendChild(titleRow);

        const promptSection = h('div', 'cycle-prompt');
        promptSection.appendChild(h('label', 'field-label', 'Prompt'));
        promptSection.appendChild(h('p', 'cycle-prompt__text', cycle.prompt));
        header.appendChild(promptSection);

        const meta = h('div', 'detail-meta');
        meta.appendChild(h('span', 'meta-item', `Created: ${formatDate(cycle.created_at)}`));
        meta.appendChild(h('span', 'meta-item', `Updated: ${formatDate(cycle.updated_at)}`));
        header.appendChild(meta);

        if (cycle.failure_reason) {
            const a = h('div', 'alert alert--error');
            a.appendChild(h('strong', '', 'Failed: '));
            a.appendChild(document.createTextNode(cycle.failure_reason));
            header.appendChild(a);
        }
        if (cycle.block_reason) {
            const a = h('div', 'alert alert--warning');
            a.appendChild(h('strong', '', 'Blocked: '));
            a.appendChild(document.createTextNode(cycle.block_reason));
            header.appendChild(a);
        }
        if (cycle.cancel_reason) {
            const a = h('div', 'alert alert--info');
            a.appendChild(h('strong', '', 'Cancelled: '));
            a.appendChild(document.createTextNode(cycle.cancel_reason));
            header.appendChild(a);
        }

        if (cycle.state === 'plan_review') {
            const actions = h('div', 'action-buttons');
            const approveBtn = h('button', 'btn btn--primary', 'Approve Plan');
            approveBtn.addEventListener('click', () => this._handleApprove());
            actions.appendChild(approveBtn);
            header.appendChild(actions);
        }
        this.el.appendChild(header);

        // Plan
        if (cycle.plan) {
            const section = h('div', 'section');
            section.appendChild(h('h3', 'section-title', 'Plan'));
            const pre = h('pre', 'plan-viewer');
            const code = h('code', 'plan-viewer__code');
            code.textContent = typeof cycle.plan === 'string' ? cycle.plan : JSON.stringify(cycle.plan, null, 2);
            pre.appendChild(code);
            section.appendChild(pre);
            this.el.appendChild(section);
        }

        // Tasks
        const section = h('div', 'section');
        const sh = h('div', 'section-header');
        sh.appendChild(h('h3', 'section-title', 'Tasks'));
        sh.appendChild(h('span', 'section-count', `${this.cycleTasks.length}`));
        section.appendChild(sh);

        if (this.cycleTasks.length === 0) {
            section.appendChild(h('div', 'empty-state', 'No tasks in this cycle.'));
        } else {
            const table = h('table', 'data-table');
            const thead = h('thead');
            const hr = h('tr');
            ['#', 'Title', 'Phase', 'State', 'Attempts', 'Key'].forEach(c => hr.appendChild(h('th', '', c)));
            thead.appendChild(hr);
            table.appendChild(thead);

            const tbody = h('tbody');
            for (const task of this.cycleTasks) {
                const row = h('tr', 'data-table__row');
                row.appendChild(h('td', 'data-table__cell mono', String(task.ordinal)));
                row.appendChild(h('td', 'data-table__cell', truncate(task.title, 50)));
                row.appendChild(h('td', 'data-table__cell', task.phase));
                const sc = h('td', 'data-table__cell');
                sc.appendChild(stateBadge(task.state));
                if (task.failure_reason) sc.appendChild(h('span', 'block-reason', truncate(task.failure_reason, 30)));
                row.appendChild(sc);
                row.appendChild(h('td', 'data-table__cell mono', `${task.current_attempt}/${task.max_retries}`));
                row.appendChild(h('td', 'data-table__cell mono text-secondary', truncate(task.task_key, 20)));
                tbody.appendChild(row);
            }
            table.appendChild(tbody);
            section.appendChild(table);
        }
        this.el.appendChild(section);
    }

    async _handleApprove() {
        const approvedBy = prompt('Approved by (your name):');
        if (!approvedBy) return;
        try {
            await api.approvePlan(this.instanceId, this.cycleId, { approved_by: approvedBy });
            await this._fetchData();
            await store.invalidate(this.instanceId);
        } catch (e) { console.error('[cycle-detail] approve error:', e); }
    }
}

// Event Timeline
class EventTimeline {
    constructor(container, instanceId) {
        this.el = h('div', 'event-timeline');
        container.appendChild(this.el);
        this.instanceId = instanceId;
        this.events = [];
        this.filterType = '';
        this.loading = false;
        this.renderKey = '';
        this.filterTimer = null;
        this._fetchEvents();
    }

    update() { this._fetchEvents(); }

    destroy() {
        if (this.filterTimer) clearTimeout(this.filterTimer);
        this.el.remove();
    }

    async _fetchEvents() {
        if (this.loading) return;
        this.loading = true;
        try {
            const resp = await api.listEvents(this.instanceId, this.filterType || undefined, undefined, 50);
            this.events = resp.data;
        } catch (e) { console.error('[events]', e); }
        this.loading = false;
        this._render();
    }

    _render() {
        const key = JSON.stringify({
            events: this.events.map(e => e.event_id),
            filter: this.filterType, loading: this.loading,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;
        clearChildren(this.el);

        const header = h('div', 'section-header');
        header.appendChild(h('h3', 'section-title', 'Events'));
        const fc = h('div', 'event-filter');
        const input = document.createElement('input');
        input.type = 'text';
        input.placeholder = 'Filter by event type...';
        input.className = 'input';
        input.value = this.filterType;
        input.addEventListener('input', () => {
            if (this.filterTimer) clearTimeout(this.filterTimer);
            this.filterTimer = setTimeout(() => {
                this.filterType = input.value.trim();
                this._fetchEvents();
            }, 300);
        });
        fc.appendChild(input);
        header.appendChild(fc);
        this.el.appendChild(header);

        this.el.appendChild(h('div', 'section-count', `${this.events.length} events`));

        if (this.loading && this.events.length === 0) {
            this.el.appendChild(h('div', 'empty-state', 'Loading events...'));
            return;
        }
        if (this.events.length === 0) {
            this.el.appendChild(h('div', 'empty-state', 'No events found.'));
            return;
        }

        const list = h('div', 'event-list');
        for (const event of this.events) {
            const item = h('div', 'event-item');
            item.appendChild(h('div', 'event-item__seq', `#${event.seq}`));
            const info = h('div', 'event-item__info');
            const typeRow = h('div', 'event-item__type');
            typeRow.appendChild(h('span', 'event-type-badge', event.event_type));
            typeRow.appendChild(h('span', 'event-item__version', `v${event.event_version}`));
            info.appendChild(typeRow);
            const payloadStr = typeof event.payload === 'string' ? event.payload : JSON.stringify(event.payload);
            const preview = h('div', 'event-item__payload', truncate(payloadStr, 120));
            preview.title = payloadStr;
            info.appendChild(preview);
            const metaRow = h('div', 'event-item__meta');
            metaRow.appendChild(h('span', 'text-secondary', formatDate(event.occurred_at)));
            if (event.correlation_id) metaRow.appendChild(h('span', 'text-secondary', `corr: ${shortId(event.correlation_id)}`));
            if (event.idempotency_key) metaRow.appendChild(h('span', 'text-secondary', `key: ${truncate(event.idempotency_key, 20)}`));
            info.appendChild(metaRow);
            item.appendChild(info);
            list.appendChild(item);
        }
        this.el.appendChild(list);
    }
}

// ── App ──────────────────────────────────────────────────────────

class App {
    constructor() {
        this.container = document.getElementById('app');
        this.currentComponents = [];
        this.currentRoute = null;
        this.wsManager = null;
        this.unsubscribe = null;
    }

    async init() {
        if (!getToken()) { this._showLogin(); return; }

        this.unsubscribe = store.subscribe((state) => this._onState(state));
        window.addEventListener('hashchange', () => this._onRoute());
        window.addEventListener('openclaw:auth-required', () => this._showLogin());
        window.addEventListener('openclaw:maintenance', () => store.setMaintenanceMode(true));

        await store.fetchInstances();
        this._onRoute();
    }

    _parseHash() {
        const hash = location.hash.replace(/^#/, '') || '/';
        const cm = hash.match(/^\/instances\/([^/]+)\/cycles\/([^/]+)$/);
        if (cm) return { view: 'cycle-detail', instanceId: cm[1], cycleId: cm[2] };
        const im = hash.match(/^\/instances\/([^/]+)$/);
        if (im) return { view: 'instance-detail', instanceId: im[1] };
        return { view: 'instance-list' };
    }

    _onRoute() {
        const route = this._parseHash();
        if (this.currentRoute && JSON.stringify(route) === JSON.stringify(this.currentRoute)) return;
        this.currentRoute = route;

        this.currentComponents.forEach(c => c.destroy());
        this.currentComponents = [];

        const newId = route.instanceId || null;
        if (this.wsManager && (!newId || newId !== this.wsManager.instanceId)) {
            this.wsManager.destroy();
            this.wsManager = null;
        }

        clearChildren(this.container);

        switch (route.view) {
            case 'instance-list':
                this.currentComponents.push(new InstanceList(this.container));
                store.setSelectedInstance(null);
                store.setSelectedCycle(null);
                break;
            case 'instance-detail':
                this.currentComponents.push(new InstanceDetail(this.container));
                this.currentComponents.push(new EventTimeline(this.container, route.instanceId));
                store.setSelectedInstance(route.instanceId);
                store.setSelectedCycle(null);
                store.fetchInstanceDetail(route.instanceId);
                if (!this.wsManager) { this.wsManager = new WsManager(route.instanceId); this.wsManager.connect(); }
                break;
            case 'cycle-detail':
                this.currentComponents.push(new CycleDetail(this.container, route.instanceId, route.cycleId));
                store.setSelectedInstance(route.instanceId);
                store.setSelectedCycle(route.cycleId);
                store.fetchInstanceDetail(route.instanceId);
                if (!this.wsManager) { this.wsManager = new WsManager(route.instanceId); this.wsManager.connect(); }
                break;
        }

        this._onState(store.getState());
    }

    _onState(state) {
        this._updateStatusBar(state);
        for (const c of this.currentComponents) c.update(state);
    }

    _updateStatusBar(state) {
        const bar = document.getElementById('status-bar');
        if (!bar) return;
        clearChildren(bar);
        const ws = h('span', `status-indicator ${state.wsConnected ? 'status-indicator--connected' : 'status-indicator--disconnected'}`);
        ws.textContent = state.wsConnected ? 'Live' : 'Disconnected';
        bar.appendChild(ws);
        if (state.maintenanceMode) bar.appendChild(h('span', 'status-maintenance', 'Maintenance Mode'));
        if (state.error) bar.appendChild(h('span', 'status-error', state.error));
    }

    _showLogin() {
        clearChildren(this.container);
        const form = h('div', 'login-form');
        form.appendChild(h('h2', 'login-form__title', 'OpenClaw Orchestrator'));
        form.appendChild(h('p', 'login-form__subtitle', 'Enter your API token to continue.'));
        const input = document.createElement('input');
        input.type = 'password';
        input.placeholder = 'API Token';
        input.className = 'input login-form__input';
        form.appendChild(input);
        const button = h('button', 'btn btn--primary login-form__button', 'Connect');
        const doLogin = () => { const t = input.value.trim(); if (t) { setToken(t); this.init(); } };
        button.addEventListener('click', doLogin);
        input.addEventListener('keydown', (e) => { if (e.key === 'Enter') doLogin(); });
        form.appendChild(button);
        this.container.appendChild(form);
    }
}

// ── Bootstrap ────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
    const app = new App();
    app.init().catch(console.error);
});
