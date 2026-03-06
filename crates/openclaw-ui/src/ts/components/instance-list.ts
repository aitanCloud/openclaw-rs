import type { AppState, Component, Instance, InstanceViewModel, CycleProgress } from '../types';
import { TERMINAL_CYCLE_STATES } from '../types';
import { store } from '../store';
import { el, clearChildren, stateBadge, timeAgo, formatCents, truncate, delegateClick } from '../utils';

/**
 * Instance dashboard — project-oriented cards with global stats bar.
 * Route: #/
 */
export class InstanceList implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];

    constructor(container: HTMLElement) {
        this.el = el('div', 'instance-list');
        container.appendChild(this.el);

        const dispose = delegateClick(this.el, '[data-instance-id]', (target) => {
            const id = target.dataset.instanceId;
            if (id) {
                location.hash = `#/instances/${id}`;
            }
        });
        this.disposables.push(dispose);
    }

    update(state: AppState): void {
        const key = JSON.stringify(
            state.instanceList.map(i => `${i.id}:${i.state}:${i.last_heartbeat}`) +
            [...state.instances.keys()].join(','),
        );
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);

        // Global stats bar
        this.renderStatsBar(state);

        // Header
        const header = el('div', 'section-header');
        const titleGroup = el('div', 'section-header__left');
        titleGroup.appendChild(el('h2', 'section-title', 'Orchestrators'));
        titleGroup.appendChild(el('span', 'section-count', `${state.instanceList.length}`));
        header.appendChild(titleGroup);

        const newBtn = el('button', 'btn btn--primary', '+ Start New Orchestrator');
        newBtn.addEventListener('click', () => {
            window.dispatchEvent(new CustomEvent('openclaw:new-instance'));
        });
        header.appendChild(newBtn);
        this.el.appendChild(header);

        if (state.loading && state.instanceList.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'Loading orchestrators...'));
            return;
        }

        if (state.instanceList.length === 0) {
            this.el.appendChild(
                el('div', 'empty-state', 'No orchestrators running. Start one to begin development work.'),
            );
            return;
        }

        // Cards grid
        const grid = el('div', 'instance-cards');
        for (const instance of state.instanceList) {
            const vm = state.instances.get(instance.id);
            grid.appendChild(this.renderCard(instance, vm ?? null));

            // Eagerly fetch detail if not cached
            if (!vm) {
                store.fetchInstanceDetail(instance.id);
            }
        }
        this.el.appendChild(grid);
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Stats bar ────────────────────────────────────────────────

    private renderStatsBar(state: AppState): void {
        if (state.instanceList.length === 0) return;

        const bar = el('div', 'stats-bar');

        // Count active instances
        const activeCount = state.instanceList.filter(i => i.state === 'active').length;

        // Count active cycles across all cached VMs
        let activeCycles = 0;
        let totalSpent = 0;
        for (const vm of state.instances.values()) {
            activeCycles += vm.cycles.filter(c => !TERMINAL_CYCLE_STATES.includes(c.state)).length;
            totalSpent += vm.totalSpentCents;
        }

        const item1 = el('span', 'stats-bar__item');
        item1.appendChild(el('span', 'stats-bar__value', String(state.instanceList.length)));
        item1.appendChild(document.createTextNode(` orchestrator${state.instanceList.length !== 1 ? 's' : ''}`));
        bar.appendChild(item1);

        if (activeCount > 0) {
            const item2 = el('span', 'stats-bar__item');
            item2.appendChild(el('span', 'stats-bar__value', String(activeCount)));
            item2.appendChild(document.createTextNode(' active'));
            bar.appendChild(item2);
        }

        if (activeCycles > 0) {
            const item3 = el('span', 'stats-bar__item');
            item3.appendChild(el('span', 'stats-bar__value', String(activeCycles)));
            item3.appendChild(document.createTextNode(` running cycle${activeCycles !== 1 ? 's' : ''}`));
            bar.appendChild(item3);
        }

        if (totalSpent > 0) {
            const item4 = el('span', 'stats-bar__item');
            item4.appendChild(el('span', 'stats-bar__value', formatCents(totalSpent)));
            item4.appendChild(document.createTextNode(' total spend'));
            bar.appendChild(item4);
        }

        this.el.appendChild(bar);
    }

    // ── Card rendering ────────────────────────────────────────────

    private renderCard(instance: Instance, vm: InstanceViewModel | null): HTMLElement {
        const card = el('div', 'instance-card');
        card.dataset.instanceId = instance.id;

        // Header: name + state
        const header = el('div', 'instance-card__header');
        const name = el('span', 'instance-card__name', instance.name);
        header.appendChild(name);

        const stateContainer = el('span', 'instance-card__state');
        stateContainer.appendChild(stateBadge(instance.state));
        header.appendChild(stateContainer);
        card.appendChild(header);

        // Block reason banner
        if (instance.block_reason) {
            const banner = el('div', 'alert alert--warning mt-0');
            banner.style.marginTop = '0';
            banner.textContent = instance.block_reason;
            card.appendChild(banner);
        }

        // Active cycle preview (if ViewModel is cached)
        if (vm) {
            const activeCycleProgress = this.getActiveCycle(vm);
            if (activeCycleProgress) {
                card.appendChild(this.renderCyclePreview(activeCycleProgress));
            }
        }

        // Footer: health + budget + last activity
        const footer = el('div', 'instance-card__footer');

        // Health indicator
        const healthMeta = el('span', 'instance-card__meta');
        healthMeta.appendChild(this.healthDot(instance.last_heartbeat));
        healthMeta.appendChild(document.createTextNode(timeAgo(instance.last_heartbeat)));
        footer.appendChild(healthMeta);

        // Budget (if available)
        if (vm && vm.totalSpentCents > 0) {
            footer.appendChild(el('span', 'instance-card__meta', formatCents(vm.totalSpentCents)));
        }

        card.appendChild(footer);
        return card;
    }

    private renderCyclePreview(cp: CycleProgress): HTMLElement {
        const wrapper = el('div', 'instance-card__cycle');

        const label = el('div', 'instance-card__cycle-label', 'Active Cycle');
        wrapper.appendChild(label);

        const prompt = el('div', 'instance-card__cycle-prompt', truncate(cp.cycle.prompt, 80));
        wrapper.appendChild(prompt);

        const stateRow = el('div', 'instance-card__cycle-state');
        stateRow.appendChild(stateBadge(cp.cycle.state));

        if (cp.tasksTotal > 0) {
            const progress = el(
                'span',
                'text-secondary',
                `${cp.tasksPassed}/${cp.tasksTotal} tasks`,
            );
            stateRow.appendChild(progress);
        }

        wrapper.appendChild(stateRow);
        return wrapper;
    }

    // ── Helpers ────────────────────────────────────────────────────

    private getActiveCycle(vm: InstanceViewModel): CycleProgress | null {
        const active = vm.cycles.find(
            c => !TERMINAL_CYCLE_STATES.includes(c.state),
        );
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

    private healthDot(heartbeat: string): HTMLElement {
        const diff = Date.now() - new Date(heartbeat).getTime();
        let cls = 'health-dot health-dot--stale';
        if (diff < 60_000) cls = 'health-dot health-dot--good';
        else if (diff < 300_000) cls = 'health-dot health-dot--warn';
        else if (diff < 600_000) cls = 'health-dot health-dot--bad';

        return el('span', cls);
    }
}
