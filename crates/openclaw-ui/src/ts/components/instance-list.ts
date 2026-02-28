import type { AppState, Component, Instance, InstanceViewModel, CycleProgress } from '../types';
import { TERMINAL_CYCLE_STATES, CYCLE_STEP_LABELS } from '../types';
import { store } from '../store';
import { el, clearChildren, stateBadge, timeAgo, formatCents, truncate, delegateClick } from '../utils';

/**
 * Instance dashboard — project-oriented cards instead of a flat table.
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

        // Header
        const header = el('div', 'section-header');
        const titleGroup = el('div', 'section-header__left');
        titleGroup.appendChild(el('h2', 'section-title', 'Instances'));
        titleGroup.appendChild(el('span', 'section-count', `${state.instanceList.length}`));
        header.appendChild(titleGroup);

        const newInstanceBtn = el('button', 'btn btn--primary', '+ New Instance');
        newInstanceBtn.addEventListener('click', () => {
            window.dispatchEvent(new CustomEvent('openclaw:new-instance'));
        });
        header.appendChild(newInstanceBtn);
        this.el.appendChild(header);

        if (state.loading && state.instanceList.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'Loading instances...'));
            return;
        }

        if (state.instanceList.length === 0) {
            this.el.appendChild(
                el('div', 'empty-state', 'No instances deployed. Use the CLI to create one.'),
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
