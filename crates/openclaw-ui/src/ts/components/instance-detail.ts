import type { AppState, Component, Cycle, InstanceViewModel, CycleProgress, Task } from '../types';
import { TERMINAL_CYCLE_STATES, CYCLE_STEPS, CYCLE_STEP_LABELS } from '../types';
import {
    el,
    clearChildren,
    stateBadge,
    formatDate,
    formatCents,
    timeAgo,
    shortId,
    truncate,
    delegateClick,
} from '../utils';

/**
 * Instance detail view — project-centric layout with active cycle panel,
 * state stepper, cycle history, and stats.
 * Route: #/instances/:id
 */
export class InstanceDetail implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];

    constructor(container: HTMLElement) {
        this.el = el('div', 'instance-detail');
        container.appendChild(this.el);

        // Delegate clicks on cycle rows and action buttons
        const dispose = delegateClick(this.el, '[data-cycle-id]', (target) => {
            const cycleId = target.dataset.cycleId;
            const instanceId = target.dataset.instanceId;
            if (cycleId && instanceId) {
                location.hash = `#/instances/${instanceId}/cycles/${cycleId}`;
            }
        });
        this.disposables.push(dispose);

        const actionDispose = delegateClick(this.el, '[data-action]', (target) => {
            const action = target.dataset.action;
            if (action === 'new-cycle') {
                window.dispatchEvent(new CustomEvent('openclaw:new-cycle'));
            }
        });
        this.disposables.push(actionDispose);
    }

    update(state: AppState): void {
        const instanceId = state.selectedInstanceId;
        if (!instanceId) return;

        const vm = state.instances.get(instanceId);
        if (!vm) {
            const key = `loading:${instanceId}`;
            if (key === this.renderKey) return;
            this.renderKey = key;
            clearChildren(this.el);
            this.el.appendChild(el('div', 'empty-state', 'Loading instance...'));
            return;
        }

        const key = JSON.stringify({
            id: vm.instance.id,
            state: vm.instance.state,
            hb: vm.instance.last_heartbeat,
            cycles: vm.cycles.map(c => `${c.id}:${c.state}`),
            budget: vm.totalSpentCents,
            tasks: vm.tasks.map(t => `${t.id}:${t.state}`),
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);
        this.renderHeader(vm);
        this.renderActiveCyclePanel(vm);
        this.renderStatsRow(vm);
        this.renderCycleHistory(vm);
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Header ────────────────────────────────────────────────────

    private renderHeader(vm: InstanceViewModel): void {
        const header = el('div', 'detail-header');

        const back = el('a', 'back-link', 'All Instances');
        (back as HTMLAnchorElement).href = '#/';
        header.appendChild(back);

        const titleRow = el('div', 'detail-title-row');
        titleRow.appendChild(el('h2', 'detail-title', vm.instance.name));
        titleRow.appendChild(stateBadge(vm.instance.state));
        header.appendChild(titleRow);

        const meta = el('div', 'detail-meta');
        meta.appendChild(this.metaItem('ID', shortId(vm.instance.id)));
        meta.appendChild(this.metaItem('Project', shortId(vm.instance.project_id)));
        meta.appendChild(this.metaItem('Started', formatDate(vm.instance.started_at)));

        const hbMeta = el('span', 'meta-item');
        hbMeta.appendChild(this.healthDot(vm.instance.last_heartbeat));
        hbMeta.appendChild(document.createTextNode(` ${timeAgo(vm.instance.last_heartbeat)}`));
        meta.appendChild(hbMeta);

        header.appendChild(meta);

        if (vm.instance.block_reason) {
            const banner = el('div', 'alert alert--warning');
            banner.appendChild(el('strong', '', 'Blocked: '));
            banner.appendChild(document.createTextNode(vm.instance.block_reason));
            header.appendChild(banner);
        }

        this.el.appendChild(header);
    }

    // ── Active Cycle Panel ────────────────────────────────────────

    private renderActiveCyclePanel(vm: InstanceViewModel): void {
        const cp = this.getActiveCycleProgress(vm);
        if (!cp) return;

        const panel = el('div', 'active-cycle-panel');

        const label = el('div', 'active-cycle-panel__label', 'Active Cycle');
        panel.appendChild(label);

        // Prompt quote
        const promptDiv = el('div', 'active-cycle-panel__prompt');
        const quote = el('div', 'prompt-quote', cp.cycle.prompt);
        promptDiv.appendChild(quote);
        panel.appendChild(promptDiv);

        // State stepper
        panel.appendChild(this.renderStepper(cp.cycle));

        // Task progress bar
        if (cp.tasksTotal > 0) {
            panel.appendChild(this.renderTaskProgress(cp));
        }

        // Action buttons
        const actions = el('div', 'active-cycle-panel__actions');

        if (cp.cycle.state === 'plan_review') {
            const approveBtn = el('button', 'btn btn--primary', 'Approve Plan');
            approveBtn.dataset.cycleId = cp.cycle.id;
            approveBtn.dataset.instanceId = vm.instance.id;
            approveBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                location.hash = `#/instances/${vm.instance.id}/cycles/${cp.cycle.id}`;
            });
            actions.appendChild(approveBtn);
        }

        const viewBtn = el('button', 'btn btn--ghost', 'View Cycle');
        viewBtn.addEventListener('click', (e) => {
            e.stopPropagation();
            location.hash = `#/instances/${vm.instance.id}/cycles/${cp.cycle.id}`;
        });
        actions.appendChild(viewBtn);

        panel.appendChild(actions);
        this.el.appendChild(panel);
    }

    private renderStepper(cycle: Cycle): HTMLElement {
        const stepper = el('div', 'state-stepper');
        const currentIdx = CYCLE_STEPS.indexOf(cycle.state);

        for (let i = 0; i < CYCLE_STEPS.length; i++) {
            const step = CYCLE_STEPS[i];
            const stepEl = el('div', 'stepper-step');

            let status: string;
            if (cycle.state === 'failed' || cycle.state === 'cancelled') {
                // If failed/cancelled, mark up to where it got as completed
                status = i < currentIdx ? 'completed' : i === currentIdx ? 'current' : 'future';
            } else if (i < currentIdx) {
                status = 'completed';
            } else if (i === currentIdx) {
                status = 'current';
            } else {
                status = 'future';
            }
            stepEl.className = `stepper-step stepper-step--${status}`;

            const indicator = el('div', 'stepper-step__indicator');
            if (status === 'completed') {
                indicator.textContent = '\u2713';
            } else {
                indicator.textContent = String(i + 1);
            }
            stepEl.appendChild(indicator);

            const label = el('div', 'stepper-step__label', CYCLE_STEP_LABELS[step]);
            stepEl.appendChild(label);

            stepper.appendChild(stepEl);
        }
        return stepper;
    }

    private renderTaskProgress(cp: CycleProgress): HTMLElement {
        const bar = el('div', 'progress-bar');

        const track = el('div', 'progress-bar__track');
        const total = cp.tasksTotal || 1;

        if (cp.tasksPassed > 0) {
            const fill = el('div', 'progress-bar__fill progress-bar__fill--passed');
            fill.style.width = `${(cp.tasksPassed / total) * 100}%`;
            track.appendChild(fill);
        }
        if (cp.tasksRunning > 0) {
            const fill = el('div', 'progress-bar__fill progress-bar__fill--running');
            fill.style.width = `${(cp.tasksRunning / total) * 100}%`;
            track.appendChild(fill);
        }
        if (cp.tasksFailed > 0) {
            const fill = el('div', 'progress-bar__fill progress-bar__fill--failed');
            fill.style.width = `${(cp.tasksFailed / total) * 100}%`;
            track.appendChild(fill);
        }

        bar.appendChild(track);

        const label = el('span', 'progress-bar__label',
            `${cp.tasksPassed}/${cp.tasksTotal} tasks complete`);
        bar.appendChild(label);

        return bar;
    }

    // ── Stats Row ─────────────────────────────────────────────────

    private renderStatsRow(vm: InstanceViewModel): void {
        const cards = el('div', 'summary-cards');

        cards.appendChild(this.card(
            'Total Cycles',
            String(vm.cycles.length),
            this.cycleBreakdown(vm.cycles),
        ));

        cards.appendChild(this.card(
            'Tasks',
            String(vm.tasks.length),
            this.taskBreakdown(vm.tasks),
        ));

        cards.appendChild(this.card(
            'Budget Spent',
            formatCents(vm.totalSpentCents),
            `${vm.budgetEntries.length} ledger entries`,
        ));

        this.el.appendChild(cards);
    }

    // ── Cycle History ─────────────────────────────────────────────

    private renderCycleHistory(vm: InstanceViewModel): void {
        const section = el('div', 'section');
        const header = el('div', 'section-header');
        header.appendChild(el('h3', 'section-title', 'Cycle History'));

        const newCycleBtn = el('button', 'btn btn--primary', '+ New Cycle');
        newCycleBtn.dataset.action = 'new-cycle';
        header.appendChild(newCycleBtn);

        section.appendChild(header);

        if (vm.cycles.length === 0) {
            section.appendChild(el('div', 'empty-state', 'No cycles yet. Start one to begin development work.'));
            this.el.appendChild(section);
            return;
        }

        const table = el('table', 'data-table');
        const thead = el('thead');
        const headerRow = el('tr');
        for (const col of ['State', 'Prompt', 'Tasks', 'Created', 'Updated']) {
            headerRow.appendChild(el('th', '', col));
        }
        thead.appendChild(headerRow);
        table.appendChild(thead);

        const tbody = el('tbody');
        for (const cycle of vm.cycles) {
            const row = el('tr', 'data-table__row clickable');
            row.dataset.cycleId = cycle.id;
            row.dataset.instanceId = vm.instance.id;

            const stateCell = el('td', 'data-table__cell');
            stateCell.appendChild(stateBadge(cycle.state));
            row.appendChild(stateCell);

            row.appendChild(el('td', 'data-table__cell', truncate(cycle.prompt, 60)));

            // Task count for this cycle
            const cycleTasks = vm.tasks.filter(t => t.cycle_id === cycle.id);
            const passed = cycleTasks.filter(t => t.state === 'completed').length;
            const taskLabel = cycleTasks.length > 0 ? `${passed}/${cycleTasks.length}` : '-';
            row.appendChild(el('td', 'data-table__cell mono', taskLabel));

            row.appendChild(el('td', 'data-table__cell text-secondary', timeAgo(cycle.created_at)));
            row.appendChild(el('td', 'data-table__cell text-secondary', timeAgo(cycle.updated_at)));

            tbody.appendChild(row);
        }
        table.appendChild(tbody);
        section.appendChild(table);
        this.el.appendChild(section);
    }

    // ── Helpers ────────────────────────────────────────────────────

    private card(title: string, value: string, subtitle: string): HTMLElement {
        const card = el('div', 'summary-card');
        card.appendChild(el('div', 'summary-card__title', title));
        if (value) card.appendChild(el('div', 'summary-card__value', value));
        if (subtitle) card.appendChild(el('div', 'summary-card__subtitle', subtitle));
        return card;
    }

    private metaItem(label: string, value: string): HTMLElement {
        const span = el('span', 'meta-item');
        span.appendChild(el('span', 'text-secondary', `${label}: `));
        span.appendChild(document.createTextNode(value));
        return span;
    }

    private healthDot(heartbeat: string): HTMLElement {
        const diff = Date.now() - new Date(heartbeat).getTime();
        let cls = 'health-dot health-dot--stale';
        if (diff < 60_000) cls = 'health-dot health-dot--good';
        else if (diff < 300_000) cls = 'health-dot health-dot--warn';
        else if (diff < 600_000) cls = 'health-dot health-dot--bad';
        return el('span', cls);
    }

    private getActiveCycleProgress(vm: InstanceViewModel): CycleProgress | null {
        const active = vm.cycles.find(c => !TERMINAL_CYCLE_STATES.includes(c.state));
        if (!active) return null;

        const tasks = vm.tasks.filter(t => t.cycle_id === active.id);
        return {
            cycle: active,
            tasks,
            tasksPassed: tasks.filter(t => t.state === 'completed').length,
            tasksFailed: tasks.filter(t => t.state === 'failed').length,
            tasksRunning: tasks.filter(t => t.state === 'running').length,
            tasksTotal: tasks.length,
        };
    }

    private cycleBreakdown(cycles: Cycle[]): string {
        const states = new Map<string, number>();
        for (const c of cycles) states.set(c.state, (states.get(c.state) ?? 0) + 1);
        return [...states.entries()].map(([s, n]) => `${n} ${s}`).join(', ') || 'none';
    }

    private taskBreakdown(tasks: Task[]): string {
        const states = new Map<string, number>();
        for (const t of tasks) states.set(t.state, (states.get(t.state) ?? 0) + 1);
        return [...states.entries()].map(([s, n]) => `${n} ${s}`).join(', ') || 'none';
    }
}
