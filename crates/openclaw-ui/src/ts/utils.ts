// ── DOM helpers ──────────────────────────────────────────────────

/** Create an element with optional class and text content. */
export function el(
    tag: string,
    className?: string,
    textContent?: string,
): HTMLElement {
    const e = document.createElement(tag);
    if (className) e.className = className;
    if (textContent !== undefined) e.textContent = textContent;
    return e;
}

/** Create an element with innerHTML (use only with trusted content). */
export function elHtml(tag: string, className: string, html: string): HTMLElement {
    const e = document.createElement(tag);
    e.className = className;
    e.innerHTML = html;
    return e;
}

/** Set multiple attributes on an element. */
export function setAttrs(
    element: HTMLElement,
    attrs: Record<string, string>,
): void {
    for (const [key, value] of Object.entries(attrs)) {
        element.setAttribute(key, value);
    }
}

/** Remove all children from an element. */
export function clearChildren(element: HTMLElement): void {
    while (element.firstChild) {
        element.removeChild(element.firstChild);
    }
}

/** Append multiple children to a parent. */
export function appendChildren(
    parent: HTMLElement,
    children: (HTMLElement | string)[],
): void {
    for (const child of children) {
        if (typeof child === 'string') {
            parent.appendChild(document.createTextNode(child));
        } else {
            parent.appendChild(child);
        }
    }
}

// ── State badge helpers ─────────────────────────────────────────

const STATE_COLORS: Record<string, string> = {
    // Instance states
    provisioning: 'blue',
    running: 'green',
    blocked: 'red',
    completed: 'gray',
    failed: 'red',
    cancelled: 'yellow',
    // Cycle states
    planning: 'blue',
    plan_review: 'purple',
    approved: 'green',
    executing: 'green',
    merging: 'blue',
    // Task states
    pending: 'gray',
    scheduled: 'blue',
    skipped: 'yellow',
};

/** Create a colored state badge pill. */
export function stateBadge(state: string): HTMLElement {
    const color = STATE_COLORS[state] || 'gray';
    const badge = el('span', `badge badge--${color}`);
    badge.textContent = state.replace(/_/g, ' ');
    return badge;
}

// ── Formatters ──────────────────────────────────────────────────

/** Format an ISO date string to a human-readable local time. */
export function formatDate(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
    });
}

/** Format a relative time string (e.g., "2m ago", "just now"). */
export function timeAgo(iso: string): string {
    const now = Date.now();
    const then = new Date(iso).getTime();
    if (isNaN(then)) return iso;
    const diff = now - then;

    if (diff < 60_000) return 'just now';
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return `${Math.floor(diff / 86_400_000)}d ago`;
}

/** Format cents to a dollar string. */
export function formatCents(cents: number): string {
    const dollars = cents / 100;
    return `$${dollars.toFixed(2)}`;
}

/** Truncate a string to maxLen, appending ellipsis if needed. */
export function truncate(s: string, maxLen: number): string {
    if (s.length <= maxLen) return s;
    return s.slice(0, maxLen - 1) + '\u2026';
}

/** Truncate a UUID to show first 8 chars. */
export function shortId(uuid: string): string {
    return uuid.slice(0, 8);
}

// ── Event delegation helper ─────────────────────────────────────

/**
 * Attach a delegated click listener on a container.
 * Returns a dispose function to remove the listener.
 */
export function delegateClick(
    container: HTMLElement,
    selector: string,
    handler: (target: HTMLElement, event: MouseEvent) => void,
): () => void {
    const listener = (e: Event) => {
        const me = e as MouseEvent;
        const target = (me.target as HTMLElement).closest(selector) as HTMLElement | null;
        if (target && container.contains(target)) {
            handler(target, me);
        }
    };
    container.addEventListener('click', listener);
    return () => container.removeEventListener('click', listener);
}
