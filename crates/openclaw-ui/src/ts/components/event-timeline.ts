import type { AppState, Component, EventRow } from '../types';
import { api } from '../api';
import { el, clearChildren, formatDate, shortId, truncate } from '../utils';

/**
 * Event timeline — filterable event list for an instance.
 * Embedded within the instance detail view or accessible via tab.
 */
export class EventTimeline implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private events: EventRow[] = [];
    private filterType: string = '';
    private loading = false;
    private instanceId: string;

    constructor(container: HTMLElement, instanceId: string) {
        this.instanceId = instanceId;
        this.el = el('div', 'event-timeline');
        container.appendChild(this.el);

        this.fetchEvents();
    }

    update(_state: AppState): void {
        // Re-fetch events when state changes (WS invalidation)
        this.fetchEvents();
    }

    destroy(): void {
        this.disposables.forEach(d => d());
        this.el.remove();
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
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);

        // Header with filter
        const header = el('div', 'section-header');
        header.appendChild(el('h3', 'section-title', 'Events'));

        const filterContainer = el('div', 'event-filter');
        const filterInput = document.createElement('input');
        filterInput.type = 'text';
        filterInput.placeholder = 'Filter by event type...';
        filterInput.className = 'input';
        filterInput.value = this.filterType;

        const handleFilter = () => {
            this.filterType = filterInput.value.trim();
            this.fetchEvents();
        };

        // Debounced filter
        let filterTimer: ReturnType<typeof setTimeout> | null = null;
        const onInput = () => {
            if (filterTimer) clearTimeout(filterTimer);
            filterTimer = setTimeout(handleFilter, 300);
        };
        filterInput.addEventListener('input', onInput);
        this.disposables.push(() => {
            filterInput.removeEventListener('input', onInput);
            if (filterTimer) clearTimeout(filterTimer);
        });

        filterContainer.appendChild(filterInput);
        header.appendChild(filterContainer);
        this.el.appendChild(header);

        // Event count
        this.el.appendChild(
            el('div', 'section-count', `${this.events.length} events`),
        );

        if (this.loading && this.events.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'Loading events...'));
            return;
        }

        if (this.events.length === 0) {
            this.el.appendChild(el('div', 'empty-state', 'No events found.'));
            return;
        }

        // Event list
        const list = el('div', 'event-list');
        for (const event of this.events) {
            list.appendChild(this.renderEvent(event));
        }
        this.el.appendChild(list);
    }

    private renderEvent(event: EventRow): HTMLElement {
        const item = el('div', 'event-item');

        // Left: sequence number
        const seqBadge = el('div', 'event-item__seq');
        seqBadge.textContent = `#${event.seq}`;
        item.appendChild(seqBadge);

        // Center: event info
        const info = el('div', 'event-item__info');
        const typeRow = el('div', 'event-item__type');

        const typeBadge = el('span', 'event-type-badge', event.event_type);
        typeRow.appendChild(typeBadge);

        const version = el('span', 'event-item__version', `v${event.event_version}`);
        typeRow.appendChild(version);

        info.appendChild(typeRow);

        // Payload preview
        const payloadStr = typeof event.payload === 'string'
            ? event.payload
            : JSON.stringify(event.payload);
        const preview = el('div', 'event-item__payload', truncate(payloadStr, 120));
        preview.title = payloadStr;
        info.appendChild(preview);

        // Metadata row
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
