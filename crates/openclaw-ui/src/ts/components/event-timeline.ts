import type { AppState, Component, EventRow } from '../types';
import { api } from '../api';
import { el, clearChildren, formatDate, shortId, truncate } from '../utils';

/**
 * Event timeline — collapsible event log for an instance.
 * Collapsed by default; expanded via toggle button.
 */
export class EventTimeline implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private events: EventRow[] = [];
    private filterType: string = '';
    private loading = false;
    private expanded = false;
    private instanceId: string;

    constructor(container: HTMLElement, instanceId: string) {
        this.instanceId = instanceId;
        this.el = el('div', 'collapsible');
        container.appendChild(this.el);

        this.render();
    }

    update(_state: AppState): void {
        // Only re-fetch if expanded
        if (this.expanded) {
            this.fetchEvents();
        }
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
    }

    // ── Toggle ────────────────────────────────────────────────────

    private toggle(): void {
        this.expanded = !this.expanded;
        if (this.expanded && this.events.length === 0) {
            this.fetchEvents();
        }
        this.render();
    }

    // ── Data fetching ────────────────────────────────────────────

    private async fetchEvents(): Promise<void> {
        if (this.loading) return;
        this.loading = true;

        try {
            const resp = await api.listEvents(
                this.instanceId,
                this.filterType || undefined,
                undefined,
                50,
            );
            this.events = resp.data;
        } catch (e) {
            console.error('[event-timeline] fetch error:', e);
        } finally {
            this.loading = false;
            this.render();
        }
    }

    // ── Rendering ────────────────────────────────────────────────

    private render(): void {
        const key = JSON.stringify({
            events: this.events.map(e => e.event_id),
            filter: this.filterType,
            loading: this.loading,
            expanded: this.expanded,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);
        this.el.className = this.expanded ? 'collapsible collapsible--open' : 'collapsible';

        // Toggle button
        const toggleBtn = document.createElement('button');
        toggleBtn.className = 'collapsible__toggle';

        const arrow = el('span', 'collapsible__arrow', '\u25B6');
        toggleBtn.appendChild(arrow);
        toggleBtn.appendChild(document.createTextNode(
            `Event Log${this.events.length > 0 ? ` (${this.events.length})` : ''}`,
        ));

        toggleBtn.addEventListener('click', () => this.toggle());
        this.disposables.push(() => toggleBtn.removeEventListener('click', () => this.toggle()));
        this.el.appendChild(toggleBtn);

        // Content (only rendered when expanded)
        const content = el('div', 'collapsible__content');

        if (this.expanded) {
            // Filter input
            const filterContainer = el('div', 'event-filter');
            filterContainer.style.marginBottom = '0.75rem';
            const filterInput = document.createElement('input');
            filterInput.type = 'text';
            filterInput.placeholder = 'Filter by event type...';
            filterInput.className = 'input';
            filterInput.value = this.filterType;

            let filterTimer: ReturnType<typeof setTimeout> | null = null;
            const onInput = () => {
                if (filterTimer) clearTimeout(filterTimer);
                filterTimer = setTimeout(() => {
                    this.filterType = filterInput.value.trim();
                    this.fetchEvents();
                }, 300);
            };
            filterInput.addEventListener('input', onInput);
            this.disposables.push(() => {
                filterInput.removeEventListener('input', onInput);
                if (filterTimer) clearTimeout(filterTimer);
            });

            filterContainer.appendChild(filterInput);
            content.appendChild(filterContainer);

            if (this.loading && this.events.length === 0) {
                content.appendChild(el('div', 'empty-state', 'Loading events...'));
            } else if (this.events.length === 0) {
                content.appendChild(el('div', 'empty-state', 'No events found.'));
            } else {
                const list = el('div', 'event-list');
                for (const event of this.events) {
                    list.appendChild(this.renderEvent(event));
                }
                content.appendChild(list);
            }
        }

        this.el.appendChild(content);
    }

    private renderEvent(event: EventRow): HTMLElement {
        const item = el('div', 'event-item');

        const seqBadge = el('div', 'event-item__seq', `#${event.seq}`);
        item.appendChild(seqBadge);

        const info = el('div', 'event-item__info');
        const typeRow = el('div', 'event-item__type');
        typeRow.appendChild(el('span', 'event-type-badge', event.event_type));
        typeRow.appendChild(el('span', 'event-item__version', `v${event.event_version}`));
        info.appendChild(typeRow);

        const payloadStr = typeof event.payload === 'string'
            ? event.payload
            : JSON.stringify(event.payload);
        const preview = el('div', 'event-item__payload', truncate(payloadStr, 120));
        preview.title = payloadStr;
        info.appendChild(preview);

        const metaRow = el('div', 'event-item__meta');
        metaRow.appendChild(el('span', 'text-secondary', formatDate(event.occurred_at)));
        if (event.correlation_id) {
            metaRow.appendChild(el('span', 'text-secondary', `corr: ${shortId(event.correlation_id)}`));
        }
        if (event.idempotency_key) {
            metaRow.appendChild(el('span', 'text-secondary', `key: ${truncate(event.idempotency_key, 20)}`));
        }
        info.appendChild(metaRow);

        item.appendChild(info);
        return item;
    }
}
