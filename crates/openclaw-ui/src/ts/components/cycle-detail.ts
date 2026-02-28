import type { AppState, Component, Cycle, Task } from '../types';
import { api } from '../api';
import { store } from '../store';
import {
    el,
    clearChildren,
    stateBadge,
    formatDate,
    shortId,
    truncate,
    delegateClick,
} from '../utils';

/**
 * Cycle detail view — shows tasks with state badges, plan viewer, approve/reject.
 * Route: #/instances/:id/cycles/:cid
 */
export class CycleDetail implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private cycle: Cycle | null = null;
    private cycleTasks: Task[] = [];
    private loading = false;

    constructor(container: HTMLElement, private instanceId: string, private cycleId: string) {
        this.el = el('div', 'cycle-detail');
        container.appendChild(this.el);

        // Delegate clicks on action buttons
        const dispose = delegateClick(this.el, '[data-action]', (target) => {
            const action = target.dataset.action;
            if (action === 'approve') this.handleApprove();
        });
        this.disposables.push(dispose);

        // Initial fetch
        this.fetchCycleData();
    }

    update(state: AppState): void {
        // If the instance view model is available, extract cycle-specific data
        const vm = state.instances.get(this.instanceId);
        if (vm) {
            const cycle = vm.cycles.find(c => c.id === this.cycleId);
            if (cycle) {
                this.cycle = cycle;
                this.cycleTasks = vm.tasks.filter(t => t.cycle_id === this.cycleId);
            }
        }
        this.render();
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Data fetching ────────────────────────────────────────────

    private async fetchCycleData(): Promise<void> {
        this.loading = true;
        this.render();

        try {
            const [cycle, tasksResp] = await Promise.all([
                api.getCycle(this.instanceId, this.cycleId),
                api.listTasks(this.instanceId, this.cycleId),
            ]);
            this.cycle = cycle;
            this.cycleTasks = tasksResp.data;
        } catch (e) {
            console.error('[cycle-detail] fetch error:', e);
        } finally {
            this.loading = false;
            this.render();
        }
    }

    // ── Rendering ────────────────────────────────────────────────

    private render(): void {
        const key = JSON.stringify({
            cycle: this.cycle ? `${this.cycle.id}:${this.cycle.state}` : null,
            tasks: this.cycleTasks.map(t => `${t.id}:${t.state}`),
            loading: this.loading,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);

        if (this.loading && !this.cycle) {
            this.el.appendChild(el('div', 'empty-state', 'Loading cycle...'));
            return;
        }

        if (!this.cycle) {
            this.el.appendChild(el('div', 'empty-state', 'Cycle not found.'));
            return;
        }

        this.renderHeader();
        this.renderPlan();
        this.renderTasks();
    }

    private renderHeader(): void {
        const cycle = this.cycle!;
        const header = el('div', 'detail-header');

        // Back link
        const back = el('a', 'back-link', 'Back to Instance');
        (back as HTMLAnchorElement).href = `#/instances/${this.instanceId}`;
        header.appendChild(back);

        const titleRow = el('div', 'detail-title-row');
        titleRow.appendChild(el('h2', 'detail-title', `Cycle ${shortId(cycle.id)}`));
        titleRow.appendChild(stateBadge(cycle.state));
        header.appendChild(titleRow);

        // Prompt
        const promptSection = el('div', 'cycle-prompt');
        promptSection.appendChild(el('label', 'field-label', 'Prompt'));
        promptSection.appendChild(el('p', 'cycle-prompt__text', cycle.prompt));
        header.appendChild(promptSection);

        // Metadata
        const meta = el('div', 'detail-meta');
        meta.appendChild(el('span', 'meta-item', `Created: ${formatDate(cycle.created_at)}`));
        meta.appendChild(el('span', 'meta-item', `Updated: ${formatDate(cycle.updated_at)}`));
        header.appendChild(meta);

        // Failure/block/cancel reasons
        if (cycle.failure_reason) {
            const alert = el('div', 'alert alert--error');
            alert.appendChild(el('strong', '', 'Failed: '));
            alert.appendChild(document.createTextNode(cycle.failure_reason));
            header.appendChild(alert);
        }
        if (cycle.block_reason) {
            const alert = el('div', 'alert alert--warning');
            alert.appendChild(el('strong', '', 'Blocked: '));
            alert.appendChild(document.createTextNode(cycle.block_reason));
            header.appendChild(alert);
        }
        if (cycle.cancel_reason) {
            const alert = el('div', 'alert alert--info');
            alert.appendChild(el('strong', '', 'Cancelled: '));
            alert.appendChild(document.createTextNode(cycle.cancel_reason));
            header.appendChild(alert);
        }

        // Action buttons (only when in plan_review state)
        if (cycle.state === 'plan_review') {
            const actions = el('div', 'action-buttons');
            const approveBtn = el('button', 'btn btn--primary', 'Approve Plan');
            approveBtn.dataset.action = 'approve';
            actions.appendChild(approveBtn);
            header.appendChild(actions);
        }

        this.el.appendChild(header);
    }

    private renderPlan(): void {
        const cycle = this.cycle!;
        if (!cycle.plan) return;

        const section = el('div', 'section');
        section.appendChild(el('h3', 'section-title', 'Plan'));

        const pre = el('pre', 'plan-viewer');
        const code = el('code', 'plan-viewer__code');
        code.textContent = typeof cycle.plan === 'string'
            ? cycle.plan
            : JSON.stringify(cycle.plan, null, 2);
        pre.appendChild(code);
        section.appendChild(pre);

        this.el.appendChild(section);
    }

    private renderTasks(): void {
        const section = el('div', 'section');
        const header = el('div', 'section-header');
        header.appendChild(el('h3', 'section-title', 'Tasks'));
        header.appendChild(el('span', 'section-count', `${this.cycleTasks.length}`));
        section.appendChild(header);

        if (this.cycleTasks.length === 0) {
            section.appendChild(el('div', 'empty-state', 'No tasks in this cycle.'));
            this.el.appendChild(section);
            return;
        }

        const table = el('table', 'data-table');
        const thead = el('thead');
        const headerRow = el('tr');
        for (const col of ['#', 'Title', 'Phase', 'State', 'Attempts', 'Key']) {
            headerRow.appendChild(el('th', '', col));
        }
        thead.appendChild(headerRow);
        table.appendChild(thead);

        const tbody = el('tbody');
        for (const task of this.cycleTasks) {
            const row = el('tr', 'data-table__row');

            row.appendChild(el('td', 'data-table__cell mono', String(task.ordinal)));
            row.appendChild(el('td', 'data-table__cell', truncate(task.title, 50)));
            row.appendChild(el('td', 'data-table__cell', task.phase));

            const stateCell = el('td', 'data-table__cell');
            stateCell.appendChild(stateBadge(task.state));
            if (task.failure_reason) {
                const reason = el('span', 'block-reason', truncate(task.failure_reason, 30));
                stateCell.appendChild(reason);
            }
            row.appendChild(stateCell);

            row.appendChild(el('td', 'data-table__cell mono', `${task.current_attempt}/${task.max_retries}`));
            row.appendChild(el('td', 'data-table__cell mono text-secondary', truncate(task.task_key, 20)));

            tbody.appendChild(row);
        }
        table.appendChild(tbody);
        section.appendChild(table);
        this.el.appendChild(section);
    }

    // ── Actions ──────────────────────────────────────────────────

    private async handleApprove(): Promise<void> {
        const approvedBy = prompt('Approved by (your name):');
        if (!approvedBy) return;

        try {
            await api.approvePlan(this.instanceId, this.cycleId, { approved_by: approvedBy });
            // Refresh data
            await this.fetchCycleData();
            await store.invalidate(this.instanceId);
        } catch (e) {
            console.error('[cycle-detail] approve error:', e);
            const msg = e instanceof Error ? e.message : String(e);
            window.dispatchEvent(new CustomEvent('openclaw:error', { detail: msg }));
        }
    }
}
