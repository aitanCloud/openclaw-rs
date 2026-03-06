import type { AppState, Component, EventRow, PaginationInfo } from '../types';
import { api } from '../api';
import { el, clearChildren, formatDate, shortId, truncate, timeAgo } from '../utils';

/**
 * Event timeline — collapsible event log for an instance.
 * Features: parsed JSON payloads, pagination, relative timestamps, event count.
 */
export class EventTimeline implements Component {
    el: HTMLElement;
    private renderKey = '';
    private disposables: (() => void)[] = [];
    private events: EventRow[] = [];
    private filterType: string = '';
    private loading = false;
    private loadingMore = false;
    private expanded = false;
    private instanceId: string;
    private pagination: PaginationInfo | null = null;
    private eventCount: number | null = null;

    constructor(container: HTMLElement, instanceId: string) {
        this.instanceId = instanceId;
        this.el = el('div', 'collapsible');
        container.appendChild(this.el);

        // Fetch event count in background for the toggle label
        this.fetchEventCount();
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

    private async fetchEventCount(): Promise<void> {
        try {
            const resp = await api.listEvents(this.instanceId, undefined, undefined, 1);
            // Approximate count from first fetch
            this.eventCount = resp.data.length + (resp.pagination.has_more ? 50 : 0);
            if (resp.data.length > 0 && resp.pagination.has_more) {
                // We know there are more than 1, show "50+" as estimate
                this.eventCount = null; // We'll show the real count after full fetch
            }
            this.render();
        } catch {
            // Ignore — count is optional
        }
    }

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
            this.pagination = resp.pagination;
            this.eventCount = resp.data.length + (resp.pagination.has_more ? 1 : 0);
        } catch (e) {
            console.error('[event-timeline] fetch error:', e);
        } finally {
            this.loading = false;
            this.render();
        }
    }

    private async loadMore(): Promise<void> {
        if (this.loadingMore || !this.pagination?.has_more || !this.pagination.next_cursor) return;
        this.loadingMore = true;
        this.render();

        try {
            const resp = await api.listEvents(
                this.instanceId,
                this.filterType || undefined,
                this.pagination.next_cursor,
                50,
            );
            this.events = [...this.events, ...resp.data];
            this.pagination = resp.pagination;
            this.eventCount = this.events.length + (resp.pagination.has_more ? 1 : 0);
        } catch (e) {
            console.error('[event-timeline] load more error:', e);
        } finally {
            this.loadingMore = false;
            this.render();
        }
    }

    // ── Rendering ────────────────────────────────────────────────

    private render(): void {
        const key = JSON.stringify({
            events: this.events.map(e => e.event_id),
            filter: this.filterType,
            loading: this.loading,
            loadingMore: this.loadingMore,
            expanded: this.expanded,
            eventCount: this.eventCount,
        });
        if (key === this.renderKey) return;
        this.renderKey = key;

        clearChildren(this.el);
        this.el.className = this.expanded ? 'collapsible collapsible--open' : 'collapsible';

        // Toggle button with event count
        const toggleBtn = document.createElement('button');
        toggleBtn.className = 'collapsible__toggle';

        const arrow = el('span', 'collapsible__arrow', '\u25B6');
        toggleBtn.appendChild(arrow);

        let toggleLabel = 'Event Log';
        if (this.events.length > 0) {
            const suffix = this.pagination?.has_more ? '+' : '';
            toggleLabel += ` (${this.events.length}${suffix})`;
        } else if (this.eventCount !== null && this.eventCount > 0) {
            toggleLabel += ` (${this.eventCount}+)`;
        }
        toggleBtn.appendChild(document.createTextNode(toggleLabel));

        toggleBtn.addEventListener('click', () => this.toggle());
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

                // Load more button
                if (this.pagination?.has_more) {
                    const moreBtn = el('button', 'load-more',
                        this.loadingMore ? 'Loading...' : 'Load more events');
                    if (!this.loadingMore) {
                        moreBtn.addEventListener('click', () => this.loadMore());
                    }
                    content.appendChild(moreBtn);
                }
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

        // Parsed payload as key-value pairs
        info.appendChild(this.renderPayload(event.payload));

        // Timestamp with relative time
        const metaRow = el('div', 'event-item__meta');
        const dateStr = formatDate(event.occurred_at);
        const relativeStr = timeAgo(event.occurred_at);
        const timeSpan = el('span', 'text-secondary');
        timeSpan.textContent = dateStr;
        const relSpan = el('span', 'event-time-relative', `\u00B7 ${relativeStr}`);
        timeSpan.appendChild(relSpan);
        metaRow.appendChild(timeSpan);

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

    /** Render event payload as parsed key-value pairs instead of raw JSON. */
    private renderPayload(payload: unknown): HTMLElement {
        if (!payload || typeof payload !== 'object') {
            const fallback = el('div', 'event-item__payload');
            fallback.textContent = payload === null ? 'null' : String(payload);
            return fallback;
        }

        const obj = payload as Record<string, unknown>;
        const keys = Object.keys(obj);

        if (keys.length === 0) {
            return el('div', 'event-item__payload', '{}');
        }

        // Show up to 4 key-value pairs
        const container = el('div', 'event-payload-kv');
        const maxKeys = 4;

        for (let i = 0; i < Math.min(keys.length, maxKeys); i++) {
            const key = keys[i];
            const value = obj[key];
            const row = el('div', 'event-payload-kv__row');
            row.appendChild(el('span', 'event-payload-kv__key', `${key}:`));

            let valueStr: string;
            if (value === null || value === undefined) {
                valueStr = 'null';
            } else if (typeof value === 'object') {
                valueStr = JSON.stringify(value);
            } else {
                valueStr = String(value);
            }

            const valueEl = el('span', 'event-payload-kv__value', truncate(valueStr, 80));
            valueEl.title = valueStr;
            row.appendChild(valueEl);
            container.appendChild(row);
        }

        if (keys.length > maxKeys) {
            const more = el('div', 'event-payload-kv__row');
            more.appendChild(el('span', 'text-secondary', `+ ${keys.length - maxKeys} more fields`));
            container.appendChild(more);
        }

        return container;
    }
}
