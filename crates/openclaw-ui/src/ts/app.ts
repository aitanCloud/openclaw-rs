import type { Component, Route, AppState } from './types';
import { store } from './store';
import { api, setToken, getToken } from './api';
import { WsManager } from './ws';
import { InstanceList } from './components/instance-list';
import { InstanceDetail } from './components/instance-detail';
import { CycleDetail } from './components/cycle-detail';
import { EventTimeline } from './components/event-timeline';
import { el, clearChildren } from './utils';

// ── Router ───────────────────────────────────────────────────────

function parseHash(): Route {
    const hash = location.hash.replace(/^#/, '') || '/';

    // #/instances/:id/cycles/:cid
    const cycleMatch = hash.match(/^\/instances\/([^/]+)\/cycles\/([^/]+)$/);
    if (cycleMatch) {
        return { view: 'cycle-detail', instanceId: cycleMatch[1], cycleId: cycleMatch[2] };
    }

    // #/instances/:id
    const instanceMatch = hash.match(/^\/instances\/([^/]+)$/);
    if (instanceMatch) {
        return { view: 'instance-detail', instanceId: instanceMatch[1] };
    }

    // Default: instance list
    return { view: 'instance-list' };
}

// ── App ──────────────────────────────────────────────────────────

class App {
    private container: HTMLElement;
    private currentComponents: Component[] = [];
    private currentRoute: Route | null = null;
    private wsManager: WsManager | null = null;
    private unsubscribe: (() => void) | null = null;

    constructor() {
        this.container = document.getElementById('app')!;
        if (!this.container) {
            throw new Error('Missing #app container');
        }
    }

    /** Initialize the application. */
    async init(): Promise<void> {
        // Check for token
        if (!getToken()) {
            this.showLoginPrompt();
            return;
        }

        // Set up store subscription
        this.unsubscribe = store.subscribe((state) => this.onStateChange(state));

        // Set up global event listeners
        window.addEventListener('hashchange', () => this.onRouteChange());
        window.addEventListener('openclaw:auth-required', () => this.showLoginPrompt());
        window.addEventListener('openclaw:maintenance', () => {
            store.setMaintenanceMode(true);
        });
        window.addEventListener('openclaw:new-cycle', () => {
            this.showCreateCycleModal();
        });

        // Initial load
        await store.fetchInstances();

        // Route to the correct view
        this.onRouteChange();
    }

    // ── Route handling ───────────────────────────────────────────

    private onRouteChange(): void {
        const route = parseHash();

        // Skip if route hasn't changed
        if (this.currentRoute && JSON.stringify(route) === JSON.stringify(this.currentRoute)) {
            return;
        }
        this.currentRoute = route;

        // Destroy old components
        this.destroyCurrentComponents();

        // Destroy old WS connection if instance changed
        const newInstanceId = 'instanceId' in route ? route.instanceId : null;
        if (this.wsManager && (!newInstanceId || newInstanceId !== this.wsManager['instanceId'])) {
            this.wsManager.destroy();
            this.wsManager = null;
        }

        // Clear container
        clearChildren(this.container);

        // Mount new components
        switch (route.view) {
            case 'instance-list':
                this.mountInstanceList();
                break;
            case 'instance-detail':
                this.mountInstanceDetail(route.instanceId);
                break;
            case 'cycle-detail':
                this.mountCycleDetail(route.instanceId, route.cycleId);
                break;
        }

        // Trigger initial render with current state
        this.onStateChange(store.getState());
    }

    private mountInstanceList(): void {
        const component = new InstanceList(this.container);
        this.currentComponents.push(component);

        store.setSelectedInstance(null);
        store.setSelectedCycle(null);
    }

    private async mountInstanceDetail(instanceId: string): Promise<void> {
        const component = new InstanceDetail(this.container);
        this.currentComponents.push(component);

        // Mount collapsible event timeline below
        const timeline = new EventTimeline(this.container, instanceId);
        this.currentComponents.push(timeline);

        store.setSelectedInstance(instanceId);
        store.setSelectedCycle(null);

        // Fetch detail data
        await store.fetchInstanceDetail(instanceId);

        // Connect WS for live updates
        if (!this.wsManager) {
            this.wsManager = new WsManager(instanceId);
            this.wsManager.connect();
        }
    }

    private async mountCycleDetail(instanceId: string, cycleId: string): Promise<void> {
        const component = new CycleDetail(this.container, instanceId, cycleId);
        this.currentComponents.push(component);

        store.setSelectedInstance(instanceId);
        store.setSelectedCycle(cycleId);

        // Ensure instance detail is loaded
        await store.fetchInstanceDetail(instanceId);

        // Connect WS for live updates
        if (!this.wsManager) {
            this.wsManager = new WsManager(instanceId);
            this.wsManager.connect();
        }
    }

    // ── State change handler ─────────────────────────────────────

    private onStateChange(state: AppState): void {
        // Update status bar
        this.updateStatusBar(state);

        // Update all mounted components
        for (const component of this.currentComponents) {
            component.update(state);
        }
    }

    private updateStatusBar(state: AppState): void {
        const statusBar = document.getElementById('status-bar');
        if (!statusBar) return;

        clearChildren(statusBar);

        // WS indicator
        const wsIndicator = el('span', `status-indicator ${state.wsConnected ? 'status-indicator--connected' : 'status-indicator--disconnected'}`);
        wsIndicator.textContent = state.wsConnected ? 'Live' : 'Disconnected';
        statusBar.appendChild(wsIndicator);

        // Maintenance banner
        if (state.maintenanceMode) {
            const banner = el('span', 'status-maintenance', 'Maintenance Mode');
            statusBar.appendChild(banner);
        }

        // Error display
        if (state.error) {
            const errorEl = el('span', 'status-error', state.error);
            statusBar.appendChild(errorEl);
        }
    }

    // ── Create Cycle Modal ───────────────────────────────────────

    private showCreateCycleModal(): void {
        const instanceId = store.getState().selectedInstanceId;
        if (!instanceId) return;

        // Create overlay
        const overlay = el('div', 'modal-overlay');

        const modal = el('div', 'modal');

        // Header
        const header = el('div', 'modal__header');
        header.appendChild(el('h3', 'modal__title', 'New Cycle'));
        const closeBtn = el('button', 'modal__close', '\u00D7');
        header.appendChild(closeBtn);
        modal.appendChild(header);

        // Body
        const body = el('div', 'modal__body');
        body.appendChild(el('label', 'field-label', 'What should this cycle accomplish?'));
        const textarea = document.createElement('textarea');
        textarea.className = 'textarea';
        textarea.placeholder = 'Describe the development task...';
        textarea.rows = 4;
        body.appendChild(textarea);
        modal.appendChild(body);

        // Error container
        const errorContainer = el('div', '');
        modal.appendChild(errorContainer);

        // Footer
        const footer = el('div', 'modal__footer');
        const cancelBtn = el('button', 'btn btn--ghost', 'Cancel');
        const startBtn = el('button', 'btn btn--primary', 'Start Cycle');
        footer.appendChild(cancelBtn);
        footer.appendChild(startBtn);
        modal.appendChild(footer);

        overlay.appendChild(modal);
        document.body.appendChild(overlay);

        // Focus textarea
        setTimeout(() => textarea.focus(), 100);

        // Handlers
        const close = () => {
            overlay.remove();
        };

        closeBtn.addEventListener('click', close);
        cancelBtn.addEventListener('click', close);
        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) close();
        });

        // Escape key
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') close();
        };
        document.addEventListener('keydown', onKeyDown);

        startBtn.addEventListener('click', async () => {
            const prompt = textarea.value.trim();
            if (!prompt) {
                textarea.style.borderColor = 'var(--accent-red)';
                return;
            }

            startBtn.textContent = 'Creating...';
            (startBtn as HTMLButtonElement).disabled = true;

            try {
                const cycle = await api.createCycle(instanceId, { prompt });
                close();
                document.removeEventListener('keydown', onKeyDown);
                // Navigate to the new cycle
                location.hash = `#/instances/${instanceId}/cycles/${cycle.id}`;
                // Refresh data
                await store.invalidate(instanceId);
            } catch (e) {
                const msg = e instanceof Error ? e.message : String(e);
                clearChildren(errorContainer);
                errorContainer.appendChild(el('div', 'modal__error', msg));
                startBtn.textContent = 'Start Cycle';
                (startBtn as HTMLButtonElement).disabled = false;
            }
        });
    }

    // ── Login prompt ─────────────────────────────────────────────

    private showLoginPrompt(): void {
        clearChildren(this.container);

        const form = el('div', 'login-form');
        form.appendChild(el('h2', 'login-form__title', 'OpenClaw Orchestrator'));
        form.appendChild(el('p', 'login-form__subtitle', 'Enter your API token to continue.'));

        const input = document.createElement('input');
        input.type = 'password';
        input.placeholder = 'API Token';
        input.className = 'input login-form__input';
        form.appendChild(input);

        const button = el('button', 'btn btn--primary login-form__button', 'Connect');
        button.addEventListener('click', () => {
            const token = input.value.trim();
            if (token) {
                setToken(token);
                this.init();
            }
        });

        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                const token = input.value.trim();
                if (token) {
                    setToken(token);
                    this.init();
                }
            }
        });

        form.appendChild(button);
        this.container.appendChild(form);
    }

    // ── Cleanup ──────────────────────────────────────────────────

    private destroyCurrentComponents(): void {
        for (const component of this.currentComponents) {
            component.destroy();
        }
        this.currentComponents = [];
    }
}

// ── Bootstrap ────────────────────────────────────────────────────

document.addEventListener('DOMContentLoaded', () => {
    const app = new App();
    app.init().catch(console.error);
});
