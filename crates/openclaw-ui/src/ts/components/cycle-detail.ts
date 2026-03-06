import type { AppState, Component, Cycle, Task, Run, CycleProgress } from '../types';
import { CYCLE_STEPS, CYCLE_STEP_LABELS, TERMINAL_CYCLE_STATES } from '../types';
import { api } from '../api';
import { store } from '../store';
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
    formatDuration,
    parsePlan,
} from '../utils';

/**
 * Cycle detail view — state stepper, plan summary/metadata,
 * structured plan with expandable tasks, task progress, actions.
 * Route: #/instances/:id/cycles/:cid
 */
export class CycleDetail implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private cycle: Cycle | null = null;
    private cycleTasks: Task[] = [];
    private loading = false;
    private expandedTasks = new Set<string>();
    private taskRuns = new Map<string, { runs: Run[]; taskState: string }>();

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
            expanded: [...this.expandedTasks],
            runs: [...this.taskRuns.entries()].map(([k, v]) => `${k}:${v.taskState}:${v.runs.length}`),
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
        this.renderPlanSummary();
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

        // For failed cycles, infer how far the cycle got
        let failedAtIdx = -1;
        if (isFailed) {
            failedAtIdx = this.inferFailedStep(cycle, this.cycleTasks);
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
        this.el.appendChild(stepper);
    }

    /** Infer which step a failed cycle reached before failing. */
    private inferFailedStep(cycle: Cycle, tasks: Task[]): number {
        const hasAnyRunningOrCompleted = tasks.some(t =>
            t.state === 'active' || t.state === 'verifying' ||
            t.state === 'passed' || t.state === 'failed',
        );
        const hasTasks = tasks.length > 0;

        if (hasAnyRunningOrCompleted) {
            return CYCLE_STEPS.indexOf('running');
        }
        if (hasTasks && cycle.plan) {
            return CYCLE_STEPS.indexOf('approved');
        }
        if (cycle.plan) {
            return CYCLE_STEPS.indexOf('plan_ready');
        }
        return CYCLE_STEPS.indexOf('planning');
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
            const retryContainer = el('div', '');
            const retryBtn = el('button', 'btn btn--primary', 'Retry with Same Prompt');
            retryBtn.dataset.action = 'retry';
            retryContainer.appendChild(retryBtn);
            retryContainer.appendChild(el('div', 'action-hint', 'Creates a new cycle with the same prompt'));
            actions.appendChild(retryContainer);
            hasActions = true;
        }

        if (!TERMINAL_CYCLE_STATES.includes(cycle.state)) {
            const cancelContainer = el('div', '');
            const cancelBtn = el('button', 'btn btn--ghost', 'Cancel Cycle');
            (cancelBtn as HTMLButtonElement).disabled = true;
            cancelBtn.style.opacity = '0.5';
            cancelContainer.appendChild(cancelBtn);
            cancelContainer.appendChild(el('div', 'action-hint', 'Cancel via CLI: openclaw cycle cancel'));
            actions.appendChild(cancelContainer);
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

        const durationCard = el('div', 'summary-card');
        durationCard.appendChild(el('div', 'summary-card__title', 'Duration'));
        durationCard.appendChild(el('div', 'summary-card__value', formatDuration(durationMs)));
        cards.appendChild(durationCard);

        // Task pass rate
        const cp = this.getCycleProgress();
        if (cp.tasksTotal === 0) {
            // Failed before tasks were created
            const passCard = el('div', 'summary-card');
            passCard.appendChild(el('div', 'summary-card__title', 'Tasks'));
            passCard.appendChild(el('div', 'summary-card__value', 'None'));
            passCard.appendChild(el('div', 'summary-card__subtitle', 'No tasks were created'));
            cards.appendChild(passCard);
        } else {
            const passRate = Math.round((cp.tasksPassed / cp.tasksTotal) * 100);
            const passCard = el('div', 'summary-card');
            passCard.appendChild(el('div', 'summary-card__title', 'Tasks Passed'));
            passCard.appendChild(el('div', 'summary-card__value', `${cp.tasksPassed}/${cp.tasksTotal}`));
            passCard.appendChild(el('div', 'summary-card__subtitle', `${passRate}% pass rate`));
            cards.appendChild(passCard);
        }

        // Outcome
        const outcomeCard = el('div', 'summary-card');
        outcomeCard.appendChild(el('div', 'summary-card__title', 'Outcome'));
        outcomeCard.appendChild(el('div', 'summary-card__value',
            cycle.state === 'completed' ? 'Success' : 'Failed'));
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

    // ── Plan Summary / Metadata ───────────────────────────────────

    private renderPlanSummary(): void {
        const cycle = this.cycle!;
        if (!cycle.plan) return;

        const plan = parsePlan(cycle.plan);
        if (!plan) return;

        const section = el('div', 'plan-summary');

        // Summary quote
        section.appendChild(el('div', 'plan-summary__quote', plan.summary));

        // Metadata row
        const meta = el('div', 'plan-summary__meta');

        if (plan.metadata?.model_id) {
            const item = el('span', 'plan-summary__meta-item');
            item.appendChild(document.createTextNode('Model: '));
            item.appendChild(el('span', 'plan-summary__meta-value', plan.metadata.model_id));
            meta.appendChild(item);
        }

        if (plan.estimated_cost > 0) {
            const item = el('span', 'plan-summary__meta-item');
            item.appendChild(document.createTextNode('Est. cost: '));
            item.appendChild(el('span', 'plan-summary__meta-value', formatCents(plan.estimated_cost)));
            meta.appendChild(item);
        }

        if (plan.tasks.length > 0) {
            const item = el('span', 'plan-summary__meta-item');
            item.appendChild(el('span', 'plan-summary__meta-value', String(plan.tasks.length)));
            item.appendChild(document.createTextNode(' tasks'));
            meta.appendChild(item);
        }

        if (plan.metadata?.generated_at) {
            const item = el('span', 'plan-summary__meta-item');
            item.appendChild(document.createTextNode('Generated: '));
            item.appendChild(el('span', 'plan-summary__meta-value', timeAgo(plan.metadata.generated_at)));
            meta.appendChild(item);
        }

        section.appendChild(meta);
        this.el.appendChild(section);
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

        if (planObj && typeof planObj === 'object' && this.cycleTasks.length > 0) {
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

    private renderStructuredPlan(plan: unknown): HTMLElement {
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

    // ── Task Items (expandable) ───────────────────────────────────

    private renderTaskItem(task: Task): HTMLElement {
        const isExpanded = this.expandedTasks.has(task.id);
        const item = el('div', `task-item task-item--expandable${isExpanded ? ' task-item--expanded' : ''}`);

        // Expand icon
        const expandIcon = el('div', 'task-item__expand-icon', '\u25B6');
        item.appendChild(expandIcon);

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

        // Only show attempt info for non-scheduled tasks
        if (task.max_retries > 1 && task.state !== 'scheduled') {
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
            meta.appendChild(el('span', 'mono text-secondary', `Key: ${truncate(task.task_key, 24)}`));
        }
        body.appendChild(meta);

        // Failure reason
        if (task.failure_reason) {
            body.appendChild(el('div', 'task-item__failure', task.failure_reason));
        }

        // Expanded detail
        if (isExpanded) {
            // Re-fetch runs if task state changed since last fetch
            this.fetchRunsIfNeeded(task);
            body.appendChild(this.renderTaskDetail(task));
        }

        item.appendChild(body);

        // Click to toggle expansion
        item.addEventListener('click', () => {
            if (this.expandedTasks.has(task.id)) {
                this.expandedTasks.delete(task.id);
            } else {
                this.expandedTasks.add(task.id);
                // Lazy-fetch runs when expanding (or re-fetch if task state changed)
                this.fetchRunsIfNeeded(task);
            }
            this.renderKey = ''; // force re-render
            this.render();
        });

        return item;
    }

    private fetchRunsIfNeeded(task: Task): void {
        const cached = this.taskRuns.get(task.id);
        // Fetch if: never fetched, or task state changed since last fetch
        if (!cached || cached.taskState !== task.state) {
            api.listRunsForTask(this.instanceId, task.id).then(runs => {
                this.taskRuns.set(task.id, { runs, taskState: task.state });
                this.renderKey = ''; // force re-render
                this.render();
            }).catch(e => console.error('[cycle-detail] fetch runs error:', e));
        }
    }

    private renderTaskDetail(task: Task): HTMLElement {
        const detail = el('div', 'task-item__detail');
        const cached = this.taskRuns.get(task.id);
        const runs = cached?.runs || [];

        const latestCompletedRun = [...runs].reverse().find(r => r.state === 'completed' || r.state === 'failed');
        const toolOutputs = latestCompletedRun?.output_json?.tool_outputs;
        const hasToolOutputs = toolOutputs && toolOutputs.length > 0;

        // ── Section A: Command Output (ground truth) ──
        if (hasToolOutputs) {
            const cmdSection = el('div', 'task-detail-section');
            const cmdHeader = el('div', 'task-detail-section__title');
            cmdHeader.textContent = 'Command Output ';
            cmdHeader.appendChild(el('span', 'badge badge--green truth-badge', 'ground truth'));
            cmdSection.appendChild(cmdHeader);

            for (const to of toolOutputs!) {
                const block = el('div', 'tool-output-block');

                if (to.command) {
                    block.appendChild(el('div', 'tool-output-command', `$ ${to.command}`));
                } else {
                    block.appendChild(el('div', 'tool-output-command', `[${to.tool_name}]`));
                }

                if (to.stdout) {
                    const pre = el('pre', 'tool-output-stdout');
                    pre.textContent = to.stdout;
                    pre.addEventListener('click', (e) => e.stopPropagation());
                    block.appendChild(pre);
                }

                if (to.stderr) {
                    block.appendChild(el('div', 'tool-output-stderr-label', 'stderr:'));
                    const stderrPre = el('pre', 'tool-output-stderr');
                    stderrPre.textContent = to.stderr;
                    stderrPre.addEventListener('click', (e) => e.stopPropagation());
                    block.appendChild(stderrPre);
                }

                cmdSection.appendChild(block);
            }

            if (latestCompletedRun?.output_json?.stdout_sha256) {
                const sha = el('div', 'tool-output-sha256');
                sha.textContent = `SHA256: ${latestCompletedRun.output_json.stdout_sha256}`;
                sha.title = 'Immutability proof \u2014 hash of concatenated raw stdout';
                sha.addEventListener('click', (e) => e.stopPropagation());
                cmdSection.appendChild(sha);
            }

            detail.appendChild(cmdSection);
        }

        // ── Section B: AI Commentary ──
        const outputSection = el('div', 'task-detail-section');
        const commentaryTitle = el('div', 'task-detail-section__title');

        if (hasToolOutputs) {
            commentaryTitle.textContent = 'AI Commentary';
        } else if (latestCompletedRun?.output_json?.output_format === 'compact') {
            commentaryTitle.textContent = 'Output ';
            commentaryTitle.appendChild(el('span', 'compact-format-notice', '(raw command output not available)'));
        } else {
            commentaryTitle.textContent = 'Output';
        }
        outputSection.appendChild(commentaryTitle);

        if (latestCompletedRun?.output_json?.result) {
            const cssClass = hasToolOutputs ? 'task-output task-output--commentary' : 'task-output';
            const pre = el('pre', cssClass);
            pre.textContent = latestCompletedRun.output_json.result;
            pre.addEventListener('click', (e) => e.stopPropagation());
            outputSection.appendChild(pre);
        } else if (task.state === 'active' || task.state === 'verifying') {
            outputSection.appendChild(el('div', 'text-secondary', 'Running...'));
        } else if (task.state === 'scheduled') {
            outputSection.appendChild(el('div', 'text-secondary', 'Waiting to start...'));
        } else if (runs.length === 0 && (task.state === 'passed' || task.state === 'failed')) {
            outputSection.appendChild(el('div', 'text-secondary', 'Loading runs...'));
        } else if (task.failure_reason) {
            outputSection.appendChild(el('div', 'task-item__failure', task.failure_reason));
        } else {
            outputSection.appendChild(el('div', 'text-secondary', 'No output available'));
        }
        detail.appendChild(outputSection);

        // ── Section B: Acceptance Criteria ──
        if (task.acceptance && Array.isArray(task.acceptance) && task.acceptance.length > 0) {
            const accSection = el('div', 'task-detail-section');
            accSection.appendChild(el('div', 'task-detail-section__title', 'Acceptance Criteria'));
            const list = el('ul', 'task-acceptance');
            for (const criterion of task.acceptance) {
                const item = el('li', 'task-acceptance__item');
                const icon = el('span', 'task-acceptance__icon');
                if (task.state === 'passed') {
                    icon.className = 'task-acceptance__icon task-acceptance__icon--pass';
                    icon.textContent = '\u2713';
                } else if (task.state === 'failed') {
                    icon.className = 'task-acceptance__icon task-acceptance__icon--fail';
                    icon.textContent = '\u2717';
                } else {
                    icon.className = 'task-acceptance__icon task-acceptance__icon--pending';
                    icon.textContent = '\u25CB';
                }
                item.appendChild(icon);
                item.appendChild(el('span', '', String(criterion)));
                list.appendChild(item);
            }
            accSection.appendChild(list);
            detail.appendChild(accSection);
        }

        // ── Section C: Run History ──
        if (runs.length > 0) {
            const runsSection = el('div', 'task-detail-section');
            runsSection.appendChild(el('div', 'task-detail-section__title', `Runs (${runs.length})`));
            const runsList = el('div', 'task-runs');
            for (const run of runs) {
                const row = el('div', 'task-run-row');
                row.appendChild(el('span', 'task-run-row__num', `#${run.run_number}`));
                row.appendChild(stateBadge(run.state));

                // Duration
                if (run.started_at && run.finished_at) {
                    const dur = new Date(run.finished_at).getTime() - new Date(run.started_at).getTime();
                    row.appendChild(el('span', 'task-run-row__meta', formatDuration(dur)));
                } else if (run.started_at && !run.finished_at) {
                    row.appendChild(el('span', 'task-run-row__meta', 'running'));
                }

                // Cost
                if (run.cost_cents > 0) {
                    row.appendChild(el('span', 'task-run-row__meta', formatCents(run.cost_cents)));
                }

                // Tokens
                if (run.output_json?.input_tokens || run.output_json?.output_tokens) {
                    const inTok = run.output_json.input_tokens || 0;
                    const outTok = run.output_json.output_tokens || 0;
                    row.appendChild(el('span', 'task-run-row__meta',
                        `${(inTok / 1000).toFixed(1)}k in / ${(outTok / 1000).toFixed(1)}k out`));
                }

                // Exit code for failures
                if (run.exit_code !== null && run.exit_code !== 0) {
                    row.appendChild(el('span', 'task-run-row__meta', `exit ${run.exit_code}`));
                }

                runsList.appendChild(row);
            }
            runsSection.appendChild(runsList);
            detail.appendChild(runsSection);
        }

        // ── Section D: Metadata ──
        const metaSection = el('div', 'task-detail-section');
        metaSection.appendChild(el('div', 'task-detail-section__title', 'Details'));
        const rows: [string, string][] = [
            ['Task ID', shortId(task.id)],
            ['Key', task.task_key],
            ['Phase', task.phase],
            ['Ordinal', String(task.ordinal)],
            ['Attempt', `${Math.max(1, task.current_attempt)} / ${task.max_retries}`],
            ['Created', formatDate(task.created_at)],
            ['Updated', formatDate(task.updated_at)],
        ];

        if (task.cancel_reason) rows.push(['Cancel Reason', task.cancel_reason]);
        if (task.skip_reason) rows.push(['Skip Reason', task.skip_reason]);

        for (const [label, value] of rows) {
            const row = el('div', 'task-item__detail-row');
            row.appendChild(el('span', 'task-item__detail-label', label));
            row.appendChild(el('span', 'task-item__detail-value', value));
            metaSection.appendChild(row);
        }
        detail.appendChild(metaSection);

        return detail;
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

    // ── Actions ────────────────────────────────────────────────────

    private async handleApprove(): Promise<void> {
        if (!this.cycle) return;
        window.dispatchEvent(new CustomEvent('openclaw:approve-plan', {
            detail: {
                instanceId: this.instanceId,
                cycleId: this.cycleId,
                cycle: this.cycle,
            },
        }));
    }

    private async handleRetry(): Promise<void> {
        if (!this.cycle) return;
        const cyclePrompt = this.cycle.prompt;
        if (!confirm(`Create a new cycle with the same prompt?\n\n"${cyclePrompt}"`)) return;

        try {
            const result = await api.createCycle(this.instanceId, { prompt: cyclePrompt });
            await store.invalidateNow(this.instanceId);
            location.hash = `#/instances/${this.instanceId}/cycles/${result.cycle_id}`;
        } catch (e) {
            console.error('[cycle-detail] retry error:', e);
            const msg = e instanceof Error ? e.message : String(e);
            window.dispatchEvent(new CustomEvent('openclaw:error', { detail: msg }));
        }
    }
}
