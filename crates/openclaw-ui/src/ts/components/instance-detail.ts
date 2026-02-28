import type { AppState, Component, Cycle, InstanceViewModel } from '../types';
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
 * Instance detail view — shows cycles, budget summary, and tasks overview.
 * Route: #/instances/:id
 */
export class InstanceDetail implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];

    constructor(container: HTMLElement) {
        this.el = el('div', 'instance-detail');
        container.appendChild(this.el);

        // Delegate clicks on cycle rows
        const dispose = delegateClick(this.el, '[data-cycle-id]', (target) => {
            const cycleId = target.dataset.cycleId;
            const instanceId = target.dataset.instanceId;
            if (cycleId && instanceId) {
                location.hash = `#/instances/${instanceId}/cycles/${cycleId}`;
            }
        });
        this.disposables.push(dispose);
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
            cycles: vm.cycles.map(c => `${c.id}:${c.state}`),
            budget: vm.totalSpentCents,
            tasks: vm.tasks.length,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);
        this.renderHeader(vm);
        this.renderSummaryCards(vm);
        this.renderCycleTable(vm);
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Render sections ──────────────────────────────────────────

    private renderHeader(vm: InstanceViewModel): void {
        const header = el('div', 'detail-header');

        // Back link
        const back = el('a', 'back-link', 'All Instances');
        (back as HTMLAnchorElement).href = '#/';
        header.appendChild(back);

        const titleRow = el('div', 'detail-title-row');
        titleRow.appendChild(el('h2', 'detail-title', vm.instance.name));
        titleRow.appendChild(stateBadge(vm.instance.state));
        header.appendChild(titleRow);

        const meta = el('div', 'detail-meta');
        meta.appendChild(el('span', 'meta-item', `ID: ${shortId(vm.instance.id)}`));
        meta.appendChild(el('span', 'meta-item', `Project: ${shortId(vm.instance.project_id)}`));
        meta.appendChild(el('span', 'meta-item', `Started: ${formatDate(vm.instance.started_at)}`));
        meta.appendChild(el('span', 'meta-item', `Last heartbeat: ${timeAgo(vm.instance.last_heartbeat)}`));
        header.appendChild(meta);

        if (vm.instance.block_reason) {
            const blockBanner = el('div', 'alert alert--warning');
            blockBanner.appendChild(el('strong', '', 'Blocked: '));
            blockBanner.appendChild(document.createTextNode(vm.instance.block_reason));
            header.appendChild(blockBanner);
        }

        this.el.appendChild(header);
    }

    private renderSummaryCards(vm: InstanceViewModel): void {
        const cards = el('div', 'summary-cards');

        // Cycles card
        const cycleCard = this.card(
            'Cycles',
            String(vm.cycles.length),
            this.cycleBreakdown(vm.cycles),
        );
        cards.appendChild(cycleCard);

        // Tasks card
        const taskCard = this.card(
            'Tasks',
            String(vm.tasks.length),
            this.taskBreakdown(vm),
        );
        cards.appendChild(taskCard);

        // Budget card
        const budgetCard = this.card(
            'Budget Spent',
            formatCents(vm.totalSpentCents),
            `${vm.budgetEntries.length} ledger entries`,
        );
        cards.appendChild(budgetCard);

        // WS status card
        const wsCard = this.card(
            'Live Updates',
            '', // no main value
            '',
        );
        // Will be populated by app with ws status

        this.el.appendChild(cards);
    }

    private renderCycleTable(vm: InstanceViewModel): void {
        const section = el('div', 'section');
        const header = el('div', 'section-header');
        header.appendChild(el('h3', 'section-title', 'Cycles'));
        section.appendChild(header);

        if (vm.cycles.length === 0) {
            section.appendChild(el('div', 'empty-state', 'No cycles yet.'));
            this.el.appendChild(section);
            return;
        }

        const table = el('table', 'data-table');
        const thead = el('thead');
        const headerRow = el('tr');
        for (const col of ['ID', 'State', 'Prompt', 'Created', 'Updated']) {
            headerRow.appendChild(el('th', '', col));
        }
        thead.appendChild(headerRow);
        table.appendChild(thead);

        const tbody = el('tbody');
        for (const cycle of vm.cycles) {
            const row = el('tr', 'data-table__row clickable');
            row.dataset.cycleId = cycle.id;
            row.dataset.instanceId = vm.instance.id;

            row.appendChild(el('td', 'data-table__cell mono', shortId(cycle.id)));

            const stateCell = el('td', 'data-table__cell');
            stateCell.appendChild(stateBadge(cycle.state));
            row.appendChild(stateCell);

            row.appendChild(el('td', 'data-table__cell', truncate(cycle.prompt, 60)));
            row.appendChild(el('td', 'data-table__cell text-secondary', timeAgo(cycle.created_at)));
            row.appendChild(el('td', 'data-table__cell text-secondary', timeAgo(cycle.updated_at)));

            tbody.appendChild(row);
        }
        table.appendChild(tbody);
        section.appendChild(table);
        this.el.appendChild(section);
    }

    // ── Helpers ──────────────────────────────────────────────────

    private card(title: string, value: string, subtitle: string): HTMLElement {
        const card = el('div', 'summary-card');
        card.appendChild(el('div', 'summary-card__title', title));
        if (value) {
            card.appendChild(el('div', 'summary-card__value', value));
        }
        if (subtitle) {
            card.appendChild(el('div', 'summary-card__subtitle', subtitle));
        }
        return card;
    }

    private cycleBreakdown(cycles: Cycle[]): string {
        const states = new Map<string, number>();
        for (const c of cycles) {
            states.set(c.state, (states.get(c.state) ?? 0) + 1);
        }
        return [...states.entries()]
            .map(([s, n]) => `${n} ${s}`)
            .join(', ');
    }

    private taskBreakdown(vm: InstanceViewModel): string {
        const states = new Map<string, number>();
        for (const t of vm.tasks) {
            states.set(t.state, (states.get(t.state) ?? 0) + 1);
        }
        return [...states.entries()]
            .map(([s, n]) => `${n} ${s}`)
            .join(', ');
    }
}
