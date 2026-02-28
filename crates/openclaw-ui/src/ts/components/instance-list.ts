import type { AppState, Component, Instance } from '../types';
import { el, clearChildren, stateBadge, timeAgo, delegateClick, truncate } from '../utils';

/**
 * Instance list view — shows all orchestrator instances with state badges.
 * Route: #/
 */
export class InstanceList implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];

    constructor(container: HTMLElement) {
        this.el = el('div', 'instance-list');
        container.appendChild(this.el);

        // Delegate clicks on instance rows
        const dispose = delegateClick(this.el, '[data-instance-id]', (target) => {
            const id = target.dataset.instanceId;
            if (id) {
                location.hash = `#/instances/${id}`;
            }
        });
        this.disposables.push(dispose);
    }

    update(state: AppState): void {
        const key = JSON.stringify(state.instanceList.map(i => `${i.id}:${i.state}:${i.last_heartbeat}`));
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);

        // Header
        const header = el('div', 'section-header');
        const title = el('h2', 'section-title', 'Instances');
        const count = el('span', 'section-count', `${state.instanceList.length}`);
        header.appendChild(title);
        header.appendChild(count);
        this.el.appendChild(header);

        if (state.loading && state.instanceList.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'Loading instances...'));
            return;
        }

        if (state.instanceList.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'No instances found. Create one to get started.'));
            return;
        }

        // Table
        const table = el('table', 'data-table');
        const thead = el('thead');
        const headerRow = el('tr');
        for (const col of ['Name', 'State', 'Project', 'Last Heartbeat']) {
            headerRow.appendChild(el('th', '', col));
        }
        thead.appendChild(headerRow);
        table.appendChild(thead);

        const tbody = el('tbody');
        for (const instance of state.instanceList) {
            tbody.appendChild(this.renderRow(instance));
        }
        table.appendChild(tbody);
        this.el.appendChild(table);
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    private renderRow(instance: Instance): HTMLElement {
        const row = el('tr', 'data-table__row clickable');
        row.dataset.instanceId = instance.id;

        // Name cell
        const nameCell = el('td', 'data-table__cell');
        const nameText = el('span', 'instance-name', truncate(instance.name, 40));
        const idText = el('span', 'instance-id', instance.id.slice(0, 8));
        nameCell.appendChild(nameText);
        nameCell.appendChild(idText);
        row.appendChild(nameCell);

        // State cell
        const stateCell = el('td', 'data-table__cell');
        stateCell.appendChild(stateBadge(instance.state));
        if (instance.block_reason) {
            const reason = el('span', 'block-reason', instance.block_reason);
            stateCell.appendChild(reason);
        }
        row.appendChild(stateCell);

        // Project ID cell
        const projectCell = el('td', 'data-table__cell mono', instance.project_id.slice(0, 8));
        row.appendChild(projectCell);

        // Last heartbeat cell
        const hbCell = el('td', 'data-table__cell text-secondary', timeAgo(instance.last_heartbeat));
        hbCell.title = instance.last_heartbeat;
        row.appendChild(hbCell);

        return row;
    }
}
