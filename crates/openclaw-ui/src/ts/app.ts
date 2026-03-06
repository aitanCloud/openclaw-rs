import type { Component, Route, AppState, Cycle } from './types';
import { store } from './store';
import { api, setToken, getToken, clearToken, ApiError } from './api';
import { WsManager } from './ws';
import { InstanceList } from './components/instance-list';
import { InstanceDetail } from './components/instance-detail';
import { CycleDetail } from './components/cycle-detail';
import { EventTimeline } from './components/event-timeline';
import { el, clearChildren, generateUuid, generateToken, formatCents, parsePlan } from './utils';

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
        window.addEventListener('openclaw:new-instance', () => {
            this.showStartOrchestratorModal();
        });
        window.addEventListener('openclaw:approve-plan', ((e: CustomEvent) => {
            const { instanceId, cycleId, cycle } = e.detail;
            this.showApprovePlanModal(instanceId, cycleId, cycle);
        }) as EventListener);
        window.addEventListener('openclaw:error', ((e: CustomEvent) => {
            this.showToast(e.detail || 'An error occurred', 'error');
        }) as EventListener);
        window.addEventListener('openclaw:toast', ((e: CustomEvent) => {
            const { message, type } = e.detail;
            this.showToast(message, type || 'info');
        }) as EventListener);

        // Initial load — verify token works
        try {
            await store.fetchInstances();
        } catch (e) {
            if (e instanceof ApiError && e.status === 401) {
                return; // auth-required event already fired
            }
        }

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
        const component = new InstanceDetail(this.container, instanceId);
        this.currentComponents.push(component);

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

        // Update all mounted components (with error boundary)
        for (const component of this.currentComponents) {
            try {
                component.update(state);
            } catch (e) {
                console.error('[app] component update error:', e);
            }
        }
    }

    private updateStatusBar(state: AppState): void {
        const statusBar = document.getElementById('status-bar');
        if (!statusBar) return;

        clearChildren(statusBar);

        // WS indicator — only show when viewing an instance (WS is instance-scoped)
        const viewingInstance = state.selectedInstanceId !== null;
        if (viewingInstance) {
            const wsIndicator = el('span', `status-indicator ${state.wsConnected ? 'status-indicator--connected' : 'status-indicator--disconnected'}`);
            wsIndicator.textContent = state.wsConnected ? 'Live' : 'Connecting to live feed...';
            statusBar.appendChild(wsIndicator);
        } else {
            const readyIndicator = el('span', 'status-indicator status-indicator--connected');
            readyIndicator.textContent = 'Ready';
            statusBar.appendChild(readyIndicator);
        }

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

        // Logout button
        const logoutBtn = el('button', 'btn--logout', 'Logout');
        logoutBtn.addEventListener('click', () => {
            clearToken();
            location.reload();
        });
        statusBar.appendChild(logoutBtn);
    }

    // ── Create Cycle Modal ───────────────────────────────────────

    private showCreateCycleModal(): void {
        const instanceId = store.getState().selectedInstanceId;
        if (!instanceId) return;

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

        setTimeout(() => textarea.focus(), 100);

        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') close();
        };
        document.addEventListener('keydown', onKeyDown);

        const close = () => {
            document.removeEventListener('keydown', onKeyDown);
            overlay.remove();
        };

        closeBtn.addEventListener('click', close);
        cancelBtn.addEventListener('click', close);
        overlay.addEventListener('click', (e) => {
            if (e.target === overlay) close();
        });

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
                location.hash = `#/instances/${instanceId}/cycles/${cycle.cycle_id}`;
                await store.invalidateNow(instanceId);
            } catch (e) {
                const msg = e instanceof Error ? e.message : String(e);
                clearChildren(errorContainer);
                errorContainer.appendChild(el('div', 'modal__error', msg));
                startBtn.textContent = 'Start Cycle';
                (startBtn as HTMLButtonElement).disabled = false;
            }
        });
    }

    // ── Start New Orchestrator Modal ─────────────────────────────

    private showStartOrchestratorModal(): void {
        const overlay = el('div', 'modal-overlay');
        const modal = el('div', 'modal modal--wide');

        const header = el('div', 'modal__header');
        header.appendChild(el('h3', 'modal__title', 'Start New Orchestrator'));
        const closeBtn = el('button', 'modal__close', '\u00D7');
        header.appendChild(closeBtn);
        modal.appendChild(header);

        const body = el('div', 'modal__body');

        // Project Name
        body.appendChild(el('label', 'field-label', 'Project Name'));
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'input';
        nameInput.placeholder = 'e.g., my-project';
        body.appendChild(nameInput);

        // Project Directory
        body.appendChild(el('div', 'field-spacer'));
        body.appendChild(el('label', 'field-label', 'Project Directory'));
        const dataDirInput = document.createElement('input');
        dataDirInput.type = 'text';
        dataDirInput.className = 'input';
        dataDirInput.placeholder = '/home/user/projects/my-project';
        body.appendChild(dataDirInput);

        // Initial Task
        body.appendChild(el('div', 'field-spacer'));
        body.appendChild(el('label', 'field-label', 'Initial Task'));
        const promptTextarea = document.createElement('textarea');
        promptTextarea.className = 'textarea';
        promptTextarea.placeholder = 'What should this orchestrator work on first?';
        promptTextarea.rows = 3;
        body.appendChild(promptTextarea);

        // Advanced section (collapsed)
        const autoProjectId = generateUuid();
        const autoToken = generateToken();

        const advToggle = el('button', 'advanced-toggle');
        const advArrow = el('span', 'advanced-toggle__arrow', '\u25B6');
        advToggle.appendChild(advArrow);
        advToggle.appendChild(document.createTextNode(' Advanced'));
        body.appendChild(advToggle);

        const advSection = el('div', 'advanced-section');

        advSection.appendChild(el('label', 'field-label', 'Project ID (auto-generated)'));
        const projectIdInput = document.createElement('input');
        projectIdInput.type = 'text';
        projectIdInput.className = 'input';
        projectIdInput.value = autoProjectId;
        advSection.appendChild(projectIdInput);

        advSection.appendChild(el('div', 'field-spacer'));
        advSection.appendChild(el('label', 'field-label', 'Auth Token (shown once — save this)'));
        const tokenDisplay = el('div', 'token-display');
        const tokenText = el('span', '', autoToken);
        tokenDisplay.appendChild(tokenText);
        const copyBtn = el('button', 'token-display__copy', 'Copy');
        copyBtn.addEventListener('click', () => {
            navigator.clipboard.writeText(autoToken).then(() => {
                copyBtn.textContent = 'Copied!';
                setTimeout(() => { copyBtn.textContent = 'Copy'; }, 1500);
            });
        });
        tokenDisplay.appendChild(copyBtn);
        advSection.appendChild(tokenDisplay);

        body.appendChild(advSection);

        advToggle.addEventListener('click', () => {
            const isOpen = advToggle.classList.toggle('advanced-toggle--open');
            advSection.style.display = isOpen ? 'block' : 'none';
        });

        modal.appendChild(body);

        const errorContainer = el('div', '');
        modal.appendChild(errorContainer);

        // Footer
        const footer = el('div', 'modal__footer');
        const cancelBtn = el('button', 'btn btn--ghost', 'Cancel');
        const createBtn = el('button', 'btn btn--primary', 'Start Orchestrator');
        footer.appendChild(cancelBtn);
        footer.appendChild(createBtn);
        modal.appendChild(footer);

        overlay.appendChild(modal);
        document.body.appendChild(overlay);
        setTimeout(() => nameInput.focus(), 100);

        const onKeyDown = (e: KeyboardEvent) => { if (e.key === 'Escape') close(); };
        document.addEventListener('keydown', onKeyDown);

        const close = () => {
            document.removeEventListener('keydown', onKeyDown);
            overlay.remove();
        };

        closeBtn.addEventListener('click', close);
        cancelBtn.addEventListener('click', close);
        overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });

        createBtn.addEventListener('click', async () => {
            const name = nameInput.value.trim();
            const dataDir = dataDirInput.value.trim();
            const initialPrompt = promptTextarea.value.trim();
            const projectId = projectIdInput.value.trim() || autoProjectId;
            const token = autoToken;

            // Validate
            if (!name) { nameInput.style.borderColor = 'var(--accent-red)'; return; }
            if (!dataDir) { dataDirInput.style.borderColor = 'var(--accent-red)'; return; }
            if (!initialPrompt) { promptTextarea.style.borderColor = 'var(--accent-red)'; return; }

            createBtn.textContent = 'Starting...';
            (createBtn as HTMLButtonElement).disabled = true;

            try {
                // Step 1: Create the instance
                const result = await api.createInstance({
                    name,
                    project_id: projectId,
                    data_dir: dataDir,
                    token,
                });

                // Step 2: Create the first cycle with the prompt
                try {
                    await api.createCycle(result.instance_id, { prompt: initialPrompt });
                } catch (cycleErr) {
                    // Instance created but cycle failed — still navigate
                    console.warn('[app] initial cycle creation failed:', cycleErr);
                }

                close();
                this.showToast('Orchestrator started \u2014 generating plan...', 'success');
                await store.fetchInstances();
                location.hash = `#/instances/${result.instance_id}`;
            } catch (e) {
                const msg = e instanceof Error ? e.message : String(e);
                clearChildren(errorContainer);
                errorContainer.appendChild(el('div', 'modal__error', msg));
                createBtn.textContent = 'Start Orchestrator';
                (createBtn as HTMLButtonElement).disabled = false;
            }
        });
    }

    // ── Approve Plan Modal ───────────────────────────────────────

    showApprovePlanModal(instanceId: string, cycleId: string, cycle: Cycle): void {
        const overlay = el('div', 'modal-overlay');
        const modal = el('div', 'modal modal--wide');

        const header = el('div', 'modal__header');
        header.appendChild(el('h3', 'modal__title', 'Approve Plan'));
        const closeBtn = el('button', 'modal__close', '\u00D7');
        header.appendChild(closeBtn);
        modal.appendChild(header);

        const body = el('div', 'modal__body');

        // Plan summary
        const plan = parsePlan(cycle.plan);
        const summarySection = el('div', 'approve-summary');

        if (plan) {
            const quote = el('div', 'approve-summary__quote', plan.summary);
            summarySection.appendChild(quote);

            const stats = el('div', 'approve-summary__stats');

            const taskStat = el('span', 'approve-summary__stat');
            taskStat.appendChild(el('span', 'approve-summary__stat-value', String(plan.tasks.length)));
            taskStat.appendChild(document.createTextNode(' tasks'));
            stats.appendChild(taskStat);

            if (plan.estimated_cost > 0) {
                const costStat = el('span', 'approve-summary__stat');
                costStat.appendChild(document.createTextNode('Est. '));
                costStat.appendChild(el('span', 'approve-summary__stat-value', formatCents(plan.estimated_cost)));
                stats.appendChild(costStat);
            }

            if (plan.metadata?.model_id) {
                const modelStat = el('span', 'approve-summary__stat');
                modelStat.appendChild(document.createTextNode('Model: '));
                modelStat.appendChild(el('span', 'approve-summary__stat-value', plan.metadata.model_id));
                stats.appendChild(modelStat);
            }

            summarySection.appendChild(stats);
        } else {
            summarySection.appendChild(el('div', 'text-secondary', 'Plan details not available.'));
        }

        body.appendChild(summarySection);

        // Approver name
        body.appendChild(el('label', 'field-label', 'Your Name'));
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'input';
        nameInput.placeholder = 'Who is approving this plan?';
        body.appendChild(nameInput);

        modal.appendChild(body);

        const errorContainer = el('div', '');
        modal.appendChild(errorContainer);

        const footer = el('div', 'modal__footer');
        const cancelBtn = el('button', 'btn btn--ghost', 'Cancel');
        const approveBtn = el('button', 'btn btn--primary', 'Approve');
        footer.appendChild(cancelBtn);
        footer.appendChild(approveBtn);
        modal.appendChild(footer);

        overlay.appendChild(modal);
        document.body.appendChild(overlay);
        setTimeout(() => nameInput.focus(), 100);

        const onKeyDown = (e: KeyboardEvent) => { if (e.key === 'Escape') close(); };
        document.addEventListener('keydown', onKeyDown);

        const close = () => {
            document.removeEventListener('keydown', onKeyDown);
            overlay.remove();
        };

        closeBtn.addEventListener('click', close);
        cancelBtn.addEventListener('click', close);
        overlay.addEventListener('click', (e) => { if (e.target === overlay) close(); });

        approveBtn.addEventListener('click', async () => {
            const approvedBy = nameInput.value.trim();
            if (!approvedBy) {
                nameInput.style.borderColor = 'var(--accent-red)';
                return;
            }

            approveBtn.textContent = 'Approving...';
            (approveBtn as HTMLButtonElement).disabled = true;

            try {
                await api.approvePlan(instanceId, cycleId, { approved_by: approvedBy });
                close();
                this.showToast('Plan approved \u2014 execution starting', 'success');
                await store.invalidateNow(instanceId);
            } catch (e) {
                const msg = e instanceof Error ? e.message : String(e);
                clearChildren(errorContainer);
                errorContainer.appendChild(el('div', 'modal__error', msg));
                approveBtn.textContent = 'Approve';
                (approveBtn as HTMLButtonElement).disabled = false;
            }
        });
    }

    // ── Toast Notifications ───────────────────────────────────────

    private showToast(message: string, type: 'success' | 'error' | 'info' = 'info'): void {
        let container = document.getElementById('toast-container');
        if (!container) {
            container = el('div', 'toast-container');
            container.id = 'toast-container';
            document.body.appendChild(container);
        }

        const toast = el('div', `toast toast--${type}`);
        toast.textContent = message;
        container.appendChild(toast);

        // Auto-dismiss after 4 seconds
        setTimeout(() => {
            toast.classList.add('toast--exiting');
            setTimeout(() => toast.remove(), 300);
        }, 4000);
    }

    // ── Login prompt ─────────────────────────────────────────────

    private showLoginPrompt(errorMessage?: string): void {
        clearChildren(this.container);

        const form = el('div', 'login-form');
        const logo = el('div', 'login-form__logo');
        logo.appendChild(el('span', 'top-bar__logo', 'OC'));
        form.appendChild(logo);
        form.appendChild(el('h2', 'login-form__title', 'OpenClaw Orchestrator'));
        form.appendChild(el('p', 'login-form__subtitle', 'Claude-powered software development orchestrator. Enter your API token to continue.'));

        // Error message
        const errorDiv = el('div', '');
        if (errorMessage) {
            errorDiv.appendChild(el('div', 'login-form__error', errorMessage));
        }
        form.appendChild(errorDiv);

        const input = document.createElement('input');
        input.type = 'password';
        input.placeholder = 'API Token';
        input.className = 'input login-form__input';
        form.appendChild(input);

        const button = el('button', 'btn btn--primary login-form__button', 'Connect');

        const doLogin = async () => {
            const token = input.value.trim();
            if (!token) return;

            button.textContent = 'Connecting...';
            (button as HTMLButtonElement).disabled = true;

            setToken(token);
            try {
                await api.listInstances(undefined, 1);
                // Token works — reinit
                this.init();
            } catch (e) {
                clearToken();
                clearChildren(errorDiv);
                const msg = e instanceof ApiError && e.status === 401
                    ? 'Invalid token. Please check and try again.'
                    : (e instanceof Error ? e.message : 'Connection failed');
                errorDiv.appendChild(el('div', 'login-form__error', msg));
                button.textContent = 'Connect';
                (button as HTMLButtonElement).disabled = false;
            }
        };

        button.addEventListener('click', doLogin);
        input.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') doLogin();
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
