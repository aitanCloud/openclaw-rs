import type { AppState, Component, Cycle, InstanceViewModel, CycleProgress, Task, BudgetEntry } from '../types';
import { TERMINAL_CYCLE_STATES, CYCLE_STEPS, CYCLE_STEP_LABELS } from '../types';
import { store } from '../store';
import { EventTimeline } from './event-timeline';
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
    parsePlan,
} from '../utils';

/**
 * Instance detail view — project-centric layout with active cycle panel,
 * state stepper, plan preview, cycle history, budget breakdown, and stats.
 * Route: #/instances/:id
 */
export class InstanceDetail implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private timeline: EventTimeline | null = null;
    private budgetExpanded = false;
    private budgetShowAll = false;

    constructor(container: HTMLElement, private instanceId: string) {
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
            cycles: vm.cycles.map(c => `${c.id}:${c.state}:${c.failure_reason}`),
            budget: vm.totalSpentCents,
            tasks: vm.tasks.map(t => `${t.id}:${t.state}`),
            budgetExpanded: this.budgetExpanded,
            budgetShowAll: this.budgetShowAll,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);
        this.renderHeader(vm);
        this.renderActiveCyclePanel(vm);
        this.renderLastFailedPanel(vm);
        this.renderStatsRow(vm);
        this.renderCycleHistory(vm);

        // Mount event timeline inside the detail view
        if (!this.timeline) {
            this.timeline = new EventTimeline(this.el, this.instanceId);
        } else {
            this.el.appendChild(this.timeline.el);
        }
        this.timeline.update(store.getState());
    }

    destroy(): void {
        if (this.timeline) this.timeline.destroy();
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Header ────────────────────────────────────────────────────

    private renderHeader(vm: InstanceViewModel): void {
        const header = el('div', 'detail-header');

        const back = el('a', 'back-link', 'All Orchestrators');
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

            // Guidance for blocked instances
            const guidance = el('div', 'guidance');
            guidance.textContent = 'This orchestrator is blocked and cannot process new work. Check the block reason above and resolve the issue to unblock it.';
            header.appendChild(guidance);
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
        panel.appendChild(this.renderStepper(cp.cycle, cp.tasks));

        // Task progress bar
        if (cp.tasksTotal > 0) {
            panel.appendChild(this.renderTaskProgress(cp));
        }

        // Plan preview when cycle is plan_ready
        if (cp.cycle.state === 'plan_ready' && cp.cycle.plan) {
            panel.appendChild(this.renderPlanPreview(cp.cycle));
        }

        // Action buttons
        const actions = el('div', 'active-cycle-panel__actions');

        if (cp.cycle.state === 'plan_ready') {
            const approveBtn = el('button', 'btn btn--primary', 'Approve Plan');
            approveBtn.addEventListener('click', (e) => {
                e.stopPropagation();
                window.dispatchEvent(new CustomEvent('openclaw:approve-plan', {
                    detail: { instanceId: vm.instance.id, cycleId: cp.cycle.id, cycle: cp.cycle },
                }));
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

    /** Show a prominent panel when there's no active cycle but the last cycle failed. */
    private renderLastFailedPanel(vm: InstanceViewModel): void {
        // Only show if there's no active cycle
        const hasActive = vm.cycles.some(c => !TERMINAL_CYCLE_STATES.includes(c.state));
        if (hasActive) return;

        // Find the most recent failed cycle
        const sorted = [...vm.cycles].sort((a, b) =>
            new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime());
        const lastFailed = sorted.find(c => c.state === 'failed');
        if (!lastFailed) return;

        const panel = el('div', 'failed-cycle-panel');

        const header = el('div', 'failed-cycle-panel__header');
        header.appendChild(el('span', 'failed-cycle-panel__icon', '\u2717'));
        header.appendChild(el('span', 'failed-cycle-panel__title', 'Last Cycle Failed'));
        panel.appendChild(header);

        // Failure reason — the key piece of information
        if (lastFailed.failure_reason) {
            const reason = el('div', 'failed-cycle-panel__reason');
            reason.textContent = lastFailed.failure_reason;
            panel.appendChild(reason);
        }

        // What was attempted
        const prompt = el('div', 'failed-cycle-panel__prompt');
        prompt.appendChild(el('span', 'text-secondary', 'Task: '));
        prompt.appendChild(document.createTextNode(truncate(lastFailed.prompt, 100)));
        panel.appendChild(prompt);

        // Stepper showing where it failed
        const tasks = vm.tasks.filter(t => t.cycle_id === lastFailed.id);
        panel.appendChild(this.renderStepper(lastFailed, tasks));

        // Actions
        const actions = el('div', 'failed-cycle-panel__actions');
        const viewBtn = el('button', 'btn btn--ghost', 'View Cycle Details');
        viewBtn.addEventListener('click', () => {
            location.hash = `#/instances/${vm.instance.id}/cycles/${lastFailed.id}`;
        });
        actions.appendChild(viewBtn);

        const retryBtn = el('button', 'btn btn--primary', 'Retry with Same Prompt');
        retryBtn.addEventListener('click', () => {
            window.dispatchEvent(new CustomEvent('openclaw:new-cycle', {
                detail: { prefillPrompt: lastFailed.prompt },
            }));
        });
        actions.appendChild(retryBtn);

        panel.appendChild(actions);
        this.el.appendChild(panel);
    }

    private renderPlanPreview(cycle: Cycle): HTMLElement {
        const plan = parsePlan(cycle.plan);
        const preview = el('div', 'plan-preview');

        preview.appendChild(el('div', 'plan-preview__header', 'Plan Preview'));

        if (plan) {
            preview.appendChild(el('div', 'plan-preview__summary', plan.summary));

            // Task list (compact, titles only)
            if (plan.tasks.length > 0) {
                const taskList = el('div', 'plan-preview__tasks');
                const maxShow = 5;
                for (let i = 0; i < Math.min(plan.tasks.length, maxShow); i++) {
                    taskList.appendChild(el('div', 'plan-preview__task', plan.tasks[i].title));
                }
                if (plan.tasks.length > maxShow) {
                    taskList.appendChild(el('div', 'plan-preview__task text-secondary',
                        `+ ${plan.tasks.length - maxShow} more`));
                }
                preview.appendChild(taskList);
            }

            // Footer with cost
            const footer = el('div', 'plan-preview__footer');
            if (plan.estimated_cost > 0) {
                footer.appendChild(el('span', 'plan-preview__cost',
                    `Estimated cost: ${formatCents(plan.estimated_cost)}`));
            }
            footer.appendChild(el('span', 'text-secondary',
                `${plan.tasks.length} task${plan.tasks.length !== 1 ? 's' : ''}`));
            preview.appendChild(footer);
        } else {
            preview.appendChild(el('div', 'text-secondary', 'Plan available \u2014 view cycle for details'));
        }

        return preview;
    }

    private renderStepper(cycle: Cycle, tasks: Task[]): HTMLElement {
        const stepper = el('div', 'state-stepper');
        const currentIdx = CYCLE_STEPS.indexOf(cycle.state);
        const isFailed = cycle.state === 'failed' || cycle.state === 'cancelled';
        const isCompleted = cycle.state === 'completed';

        // For failed cycles, infer how far the cycle got
        let failedAtIdx = -1;
        if (isFailed) {
            failedAtIdx = this.inferFailedStep(cycle, tasks);
        }

        for (let i = 0; i < CYCLE_STEPS.length; i++) {
            const step = CYCLE_STEPS[i];
            const stepEl = el('div', 'stepper-step');

            let status: string;
            if (isCompleted) {
                status = 'completed';
            } else if (isFailed) {
                if (i < failedAtIdx) {
                    status = 'completed';
                } else if (i === failedAtIdx) {
                    status = 'failed';
                } else {
                    status = 'future';
                }
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
            } else if (status === 'failed') {
                indicator.textContent = '\u2717';
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

    /** Infer which step a failed cycle reached before failing. */
    private inferFailedStep(cycle: Cycle, tasks: Task[]): number {
        // Check task activity to infer progress
        const hasAnyRunningOrCompleted = tasks.some(t =>
            t.state === 'active' || t.state === 'verifying' ||
            t.state === 'passed' || t.state === 'failed',
        );
        const hasTasks = tasks.length > 0;

        if (hasAnyRunningOrCompleted) {
            // Got to running phase
            return CYCLE_STEPS.indexOf('running');
        }
        if (hasTasks && cycle.plan) {
            // Had a plan and tasks were created → approved
            return CYCLE_STEPS.indexOf('approved');
        }
        if (cycle.plan) {
            // Had a plan but no tasks → failed at plan_ready or approved
            return CYCLE_STEPS.indexOf('plan_ready');
        }
        // No plan → failed during planning
        return CYCLE_STEPS.indexOf('planning');
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

        // Budget card — clickable to expand ledger
        const budgetCard = el('div', 'summary-card summary-card--clickable');
        budgetCard.appendChild(el('div', 'summary-card__title', 'Budget Spent'));
        budgetCard.appendChild(el('div', 'summary-card__value', formatCents(vm.totalSpentCents)));
        budgetCard.appendChild(el('div', 'summary-card__subtitle',
            `${vm.budgetEntries.length} ledger entries \u2014 click to expand`));
        budgetCard.addEventListener('click', () => {
            this.budgetExpanded = !this.budgetExpanded;
            this.renderKey = ''; // force re-render
            this.update(store.getState());
        });
        cards.appendChild(budgetCard);

        this.el.appendChild(cards);

        // Budget breakdown (expandable)
        if (this.budgetExpanded && vm.budgetEntries.length > 0) {
            this.el.appendChild(this.renderBudgetBreakdown(vm.budgetEntries));
        }
    }

    private renderBudgetBreakdown(entries: BudgetEntry[]): HTMLElement {
        const wrapper = el('div', 'budget-breakdown');

        const table = document.createElement('table');
        table.className = 'budget-table';

        const thead = el('thead');
        const headerRow = el('tr');
        for (const col of ['Event Type', 'Amount', 'Balance', 'Date']) {
            headerRow.appendChild(el('th', '', col));
        }
        thead.appendChild(headerRow);
        table.appendChild(thead);

        const tbody = el('tbody');
        const maxShow = this.budgetShowAll ? entries.length : 10;
        const sorted = [...entries].sort((a, b) =>
            new Date(b.created_at).getTime() - new Date(a.created_at).getTime());

        for (let i = 0; i < Math.min(sorted.length, maxShow); i++) {
            const entry = sorted[i];
            const row = el('tr');

            row.appendChild(el('td', '', entry.event_type.replace(/_/g, ' ')));

            const amountCell = el('td', '');
            const isCharge = entry.amount_cents < 0;
            amountCell.className = isCharge ? 'budget-amount--charge' : 'budget-amount--release';
            amountCell.textContent = (isCharge ? '-' : '+') + formatCents(Math.abs(entry.amount_cents));
            row.appendChild(amountCell);

            row.appendChild(el('td', 'mono', formatCents(entry.balance_after)));
            row.appendChild(el('td', 'text-secondary', timeAgo(entry.created_at)));

            tbody.appendChild(row);
        }
        table.appendChild(tbody);
        wrapper.appendChild(table);

        if (sorted.length > 10 && !this.budgetShowAll) {
            const moreBtn = el('button', 'budget-more',
                `Show all ${sorted.length} entries`);
            moreBtn.addEventListener('click', () => {
                this.budgetShowAll = true;
                this.renderKey = '';
                this.update(store.getState());
            });
            wrapper.appendChild(moreBtn);
        }

        return wrapper;
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

            const promptCell = el('td', 'data-table__cell');
            const promptText = truncate(cycle.prompt, 60);
            promptCell.title = cycle.prompt; // Full prompt on hover
            promptCell.appendChild(document.createTextNode(promptText));
            // Show failure reason inline below prompt
            if (cycle.state === 'failed' && cycle.failure_reason) {
                const failLine = el('div', 'cycle-failure-reason');
                failLine.textContent = truncate(cycle.failure_reason, 80);
                failLine.title = cycle.failure_reason;
                promptCell.appendChild(failLine);
            }
            row.appendChild(promptCell);

            // Task count for this cycle
            const cycleTasks = vm.tasks.filter(t => t.cycle_id === cycle.id);
            const passed = cycleTasks.filter(t => t.state === 'passed').length;
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
            tasksPassed: tasks.filter(t => t.state === 'passed').length,
            tasksFailed: tasks.filter(t => t.state === 'failed').length,
            tasksRunning: tasks.filter(t => t.state === 'active').length,
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
