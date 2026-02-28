import type { AppState, Component, Cycle, Task, CycleProgress } from '../types';
import { CYCLE_STEPS, CYCLE_STEP_LABELS, TERMINAL_CYCLE_STATES } from '../types';
import { api } from '../api';
import { store } from '../store';
import {
    el,
    clearChildren,
    stateBadge,
    formatDate,
    timeAgo,
    shortId,
    truncate,
    delegateClick,
} from '../utils';

/**
 * Cycle detail view — state stepper, structured plan, task progress, actions.
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

        const dispose = delegateClick(this.el, '[data-action]', (target) => {
            const action = target.dataset.action;
            if (action === 'approve') this.handleApprove();
            if (action === 'retry') this.handleRetry();
        });
        this.disposables.push(dispose);

        this.fetchCycleData();
    }

    update(state: AppState): void {
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
            cycle: this.cycle ? `${this.cycle.id}:${this.cycle.state}:${this.cycle.updated_at}` : null,
            tasks: this.cycleTasks.map(t => `${t.id}:${t.state}:${t.current_attempt}`),
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
        this.renderStepper();
        this.renderAlerts();
        this.renderCompletionSummary();
        this.renderTaskProgress();
        this.renderPlan();
    }

    // ── Header ────────────────────────────────────────────────────

    private renderHeader(): void {
        const cycle = this.cycle!;
        const header = el('div', 'detail-header');

        const back = el('a', 'back-link', 'Back to Instance');
        (back as HTMLAnchorElement).href = `#/instances/${this.instanceId}`;
        header.appendChild(back);

        const titleRow = el('div', 'detail-title-row');
        titleRow.appendChild(el('h2', 'detail-title', `Cycle ${shortId(cycle.id)}`));
        titleRow.appendChild(stateBadge(cycle.state));
        header.appendChild(titleRow);

        // Prompt as quote block
        const quote = el('div', 'prompt-quote', cycle.prompt);
        header.appendChild(quote);

        const meta = el('div', 'detail-meta');
        meta.style.marginTop = '0.75rem';
        meta.appendChild(el('span', 'meta-item', `Created ${timeAgo(cycle.created_at)}`));
        meta.appendChild(el('span', 'meta-item', `Updated ${timeAgo(cycle.updated_at)}`));
        header.appendChild(meta);

        this.el.appendChild(header);
    }

    // ── State Stepper ─────────────────────────────────────────────

    private renderStepper(): void {
        const cycle = this.cycle!;
        const stepper = el('div', 'state-stepper');
        const currentIdx = CYCLE_STEPS.indexOf(cycle.state);
        const isFailed = cycle.state === 'failed' || cycle.state === 'cancelled';
        const isCompleted = cycle.state === 'completed';

        for (let i = 0; i < CYCLE_STEPS.length; i++) {
            const step = CYCLE_STEPS[i];
            const stepEl = el('div', 'stepper-step');

            let status: string;
            if (isCompleted) {
                // All steps completed including the last one
                status = 'completed';
            } else if (isFailed) {
                // Failed/cancelled: show all as dimmed, none completed
                // (we don't know which step it failed at)
                status = 'failed';
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
        this.el.appendChild(stepper);
    }

    // ── Alerts ────────────────────────────────────────────────────

    private renderAlerts(): void {
        const cycle = this.cycle!;

        if (cycle.failure_reason) {
            const alert = el('div', 'alert alert--error');
            alert.appendChild(el('strong', '', 'Failed: '));
            alert.appendChild(document.createTextNode(cycle.failure_reason));
            this.el.appendChild(alert);
        }
        if (cycle.block_reason) {
            const alert = el('div', 'alert alert--warning');
            alert.appendChild(el('strong', '', 'Blocked: '));
            alert.appendChild(document.createTextNode(cycle.block_reason));
            this.el.appendChild(alert);
        }
        if (cycle.cancel_reason) {
            const alert = el('div', 'alert alert--info');
            alert.appendChild(el('strong', '', 'Cancelled: '));
            alert.appendChild(document.createTextNode(cycle.cancel_reason));
            this.el.appendChild(alert);
        }

        // Action buttons
        const actions = el('div', 'action-buttons');
        let hasActions = false;

        if (cycle.state === 'plan_ready') {
            const approveBtn = el('button', 'btn btn--primary', 'Approve Plan');
            approveBtn.dataset.action = 'approve';
            actions.appendChild(approveBtn);
            hasActions = true;
        }

        if (cycle.state === 'failed') {
            const retryBtn = el('button', 'btn btn--primary', 'Retry with Same Prompt');
            retryBtn.dataset.action = 'retry';
            actions.appendChild(retryBtn);
            hasActions = true;
        }

        if (!TERMINAL_CYCLE_STATES.includes(cycle.state)) {
            const cancelBtn = el('button', 'btn btn--ghost', 'Cancel Cycle');
            (cancelBtn as HTMLButtonElement).disabled = true;
            cancelBtn.title = 'Cancel via CLI: openclaw cycle cancel';
            cancelBtn.style.opacity = '0.5';
            actions.appendChild(cancelBtn);
            hasActions = true;
        }

        if (hasActions) {
            this.el.appendChild(actions);
        }
    }

    // ── Completion Summary ──────────────────────────────────────────

    private renderCompletionSummary(): void {
        const cycle = this.cycle!;
        if (cycle.state !== 'completed' && cycle.state !== 'failed') return;

        const cards = el('div', 'summary-cards');

        // Duration
        const created = new Date(cycle.created_at).getTime();
        const updated = new Date(cycle.updated_at).getTime();
        const durationMs = updated - created;
        let durationStr: string;
        if (durationMs < 3_600_000) {
            durationStr = `${Math.round(durationMs / 60_000)}m`;
        } else if (durationMs < 86_400_000) {
            durationStr = `${(durationMs / 3_600_000).toFixed(1)}h`;
        } else {
            durationStr = `${(durationMs / 86_400_000).toFixed(1)}d`;
        }
        const durationCard = el('div', 'summary-card');
        durationCard.appendChild(el('div', 'summary-card__title', 'Duration'));
        durationCard.appendChild(el('div', 'summary-card__value', durationStr));
        cards.appendChild(durationCard);

        // Task pass rate
        const cp = this.getCycleProgress();
        const passRate = cp.tasksTotal > 0 ? Math.round((cp.tasksPassed / cp.tasksTotal) * 100) : 0;
        const passCard = el('div', 'summary-card');
        passCard.appendChild(el('div', 'summary-card__title', 'Tasks Passed'));
        passCard.appendChild(el('div', 'summary-card__value', `${cp.tasksPassed}/${cp.tasksTotal}`));
        passCard.appendChild(el('div', 'summary-card__subtitle', `${passRate}% pass rate`));
        cards.appendChild(passCard);

        // Outcome
        const outcomeCard = el('div', 'summary-card');
        outcomeCard.appendChild(el('div', 'summary-card__title', 'Outcome'));
        outcomeCard.appendChild(el('div', 'summary-card__value', cycle.state === 'completed' ? 'Success' : 'Failed'));
        if (cycle.failure_reason) {
            outcomeCard.appendChild(el('div', 'summary-card__subtitle', truncate(cycle.failure_reason, 40)));
        }
        cards.appendChild(outcomeCard);

        this.el.appendChild(cards);
    }

    // ── Task Progress ─────────────────────────────────────────────

    private renderTaskProgress(): void {
        if (this.cycleTasks.length === 0) return;

        const cp = this.getCycleProgress();
        const bar = el('div', 'progress-bar');
        bar.style.marginTop = '1rem';

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
        bar.appendChild(el('span', 'progress-bar__label',
            `${cp.tasksPassed}/${cp.tasksTotal} tasks complete`));
        this.el.appendChild(bar);
    }

    // ── Structured Plan ───────────────────────────────────────────

    private renderPlan(): void {
        const cycle = this.cycle!;
        if (!cycle.plan) {
            if (cycle.state === 'planning') {
                const section = el('div', 'section');
                section.appendChild(el('h3', 'section-title', 'Plan'));
                const loadingDiv = el('div', 'empty-state', 'Generating plan...');
                section.appendChild(loadingDiv);
                this.el.appendChild(section);
            }
            return;
        }

        const section = el('div', 'section');
        const planHeader = el('div', 'section-header');
        planHeader.appendChild(el('h3', 'section-title', 'Plan'));
        if (this.cycleTasks.length > 0) {
            planHeader.appendChild(el('span', 'section-count', `${this.cycleTasks.length} tasks`));
        }
        section.appendChild(planHeader);

        // Try to render structured plan from JSON
        const planObj = typeof cycle.plan === 'string' ? this.tryParsePlan(cycle.plan) : cycle.plan;

        if (planObj && typeof planObj === 'object' && this.isStructuredPlan(planObj)) {
            section.appendChild(this.renderStructuredPlan(planObj));
        } else {
            // Fallback to raw display
            const pre = el('pre', 'plan-viewer');
            const code = el('code', 'plan-viewer__code');
            code.textContent = typeof cycle.plan === 'string'
                ? cycle.plan
                : JSON.stringify(cycle.plan, null, 2);
            pre.appendChild(code);
            section.appendChild(pre);
        }

        this.el.appendChild(section);
    }

    private renderStructuredPlan(plan: any): HTMLElement {
        const container = el('div', 'plan-structured');

        // Group tasks by phase
        const phases = new Map<string, Task[]>();
        for (const task of this.cycleTasks) {
            const phase = task.phase || 'default';
            if (!phases.has(phase)) phases.set(phase, []);
            phases.get(phase)!.push(task);
        }

        // Sort tasks within each phase by ordinal
        for (const tasks of phases.values()) {
            tasks.sort((a, b) => a.ordinal - b.ordinal);
        }

        for (const [phaseName, tasks] of phases) {
            const phaseEl = el('div', 'plan-phase');
            const header = el('div', 'plan-phase__header', phaseName);
            phaseEl.appendChild(header);

            const taskList = el('div', 'plan-phase__tasks');
            for (const task of tasks) {
                taskList.appendChild(this.renderTaskItem(task));
            }
            phaseEl.appendChild(taskList);
            container.appendChild(phaseEl);
        }

        return container;
    }

    // ── Task List ─────────────────────────────────────────────────

    private renderTaskList(): void {
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

        // Sort by ordinal
        const sorted = [...this.cycleTasks].sort((a, b) => a.ordinal - b.ordinal);

        const list = el('div', 'plan-structured');
        for (const task of sorted) {
            list.appendChild(this.renderTaskItem(task));
        }
        section.appendChild(list);
        this.el.appendChild(section);
    }

    private renderTaskItem(task: Task): HTMLElement {
        const item = el('div', 'task-item');

        // Checkbox indicator
        const check = el('div', 'task-item__check');
        switch (task.state) {
            case 'passed':
                check.className = 'task-item__check task-item__check--passed';
                check.textContent = '\u2713';
                break;
            case 'failed':
                check.className = 'task-item__check task-item__check--failed';
                check.textContent = '\u2717';
                break;
            case 'active':
            case 'verifying':
                check.className = 'task-item__check task-item__check--running';
                check.textContent = '\u25B6';
                break;
            case 'skipped':
                check.className = 'task-item__check task-item__check--skipped';
                check.textContent = '\u2192';
                break;
            default:
                check.className = 'task-item__check task-item__check--pending';
                check.textContent = '';
                break;
        }
        item.appendChild(check);

        // Body
        const body = el('div', 'task-item__body');
        body.appendChild(el('div', 'task-item__title', task.title));

        if (task.description) {
            body.appendChild(el('div', 'task-item__description', truncate(task.description, 120)));
        }

        // Meta row
        const meta = el('div', 'task-item__meta');
        meta.appendChild(stateBadge(task.state));

        if (task.max_retries > 1) {
            const attempt = Math.max(1, task.current_attempt);
            meta.appendChild(el('span', 'text-secondary',
                `Attempt ${attempt}/${task.max_retries}`));
        }

        if ((task.state === 'active' || task.state === 'verifying') && task.updated_at) {
            const elapsed = Date.now() - new Date(task.updated_at).getTime();
            const mins = Math.floor(elapsed / 60_000);
            const label = mins < 1 ? 'Just started' : `Running ${mins}m`;
            meta.appendChild(el('span', 'text-secondary', label));
        }

        if (task.task_key) {
            meta.appendChild(el('span', 'mono text-secondary', truncate(task.task_key, 24)));
        }
        body.appendChild(meta);

        // Failure reason
        if (task.failure_reason) {
            body.appendChild(el('div', 'task-item__failure', task.failure_reason));
        }

        item.appendChild(body);
        return item;
    }

    // ── Helpers ────────────────────────────────────────────────────

    private getCycleProgress(): CycleProgress {
        return {
            cycle: this.cycle!,
            tasks: this.cycleTasks,
            tasksPassed: this.cycleTasks.filter(t => t.state === 'passed').length,
            tasksFailed: this.cycleTasks.filter(t => t.state === 'failed').length,
            tasksRunning: this.cycleTasks.filter(t => t.state === 'active').length,
            tasksTotal: this.cycleTasks.length,
        };
    }

    private tryParsePlan(s: string): unknown {
        try { return JSON.parse(s); } catch { return null; }
    }

    private isStructuredPlan(obj: unknown): boolean {
        // Consider it structured if we have tasks to display in phases
        return this.cycleTasks.length > 0;
    }

    // ── Actions ────────────────────────────────────────────────────

    private async handleApprove(): Promise<void> {
        const approvedBy = prompt('Approved by (your name):');
        if (!approvedBy) return;

        try {
            await api.approvePlan(this.instanceId, this.cycleId, { approved_by: approvedBy });
            await this.fetchCycleData();
            await store.invalidate(this.instanceId);
        } catch (e) {
            console.error('[cycle-detail] approve error:', e);
            const msg = e instanceof Error ? e.message : String(e);
            window.dispatchEvent(new CustomEvent('openclaw:error', { detail: msg }));
        }
    }

    private async handleRetry(): Promise<void> {
        if (!this.cycle) return;
        const prompt = this.cycle.prompt;
        if (!confirm(`Create a new cycle with the same prompt?\n\n"${prompt}"`)) return;

        try {
            const result = await api.createCycle(this.instanceId, { prompt });
            await store.invalidate(this.instanceId);
            location.hash = `#/instances/${this.instanceId}/cycles/${result.cycle_id}`;
        } catch (e) {
            console.error('[cycle-detail] retry error:', e);
            const msg = e instanceof Error ? e.message : String(e);
            window.dispatchEvent(new CustomEvent('openclaw:error', { detail: msg }));
        }
    }
}
