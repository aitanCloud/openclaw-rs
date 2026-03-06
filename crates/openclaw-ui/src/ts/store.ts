import type { AppState, InstanceViewModel, Instance, Cycle, Task, BudgetEntry } from './types';
import { api } from './api';

// ── Listener type ────────────────────────────────────────────────

type Listener = (state: AppState) => void;

// ── Store ────────────────────────────────────────────────────────

class Store {
    private state: AppState;
    private listeners: Set<Listener> = new Set();
    private invalidateTimers: Map<string, ReturnType<typeof setTimeout>> = new Map();

    constructor() {
        this.state = {
            instances: new Map(),
            instanceList: [],
            selectedInstanceId: null,
            selectedCycleId: null,
            wsConnected: false,
            lastEventSeq: new Map(),
            maintenanceMode: false,
            loading: false,
            error: null,
        };
    }

    /** Get the current state (read-only snapshot). */
    getState(): AppState {
        return this.state;
    }

    /** Subscribe to state changes. Returns an unsubscribe function. */
    subscribe(listener: Listener): () => void {
        this.listeners.add(listener);
        return () => this.listeners.delete(listener);
    }

    /** Notify all listeners of a state change. */
    private notify(): void {
        for (const listener of this.listeners) {
            try {
                listener(this.state);
            } catch (e) {
                console.error('[store] listener error:', e);
            }
        }
    }

    /** Merge partial state and notify listeners. */
    private update(partial: Partial<AppState>): void {
        this.state = { ...this.state, ...partial };
        this.notify();
    }

    // ── Actions ──────────────────────────────────────────────────

    setLoading(loading: boolean): void {
        this.update({ loading });
    }

    setError(error: string | null): void {
        this.update({ error });
    }

    setWsConnected(connected: boolean): void {
        this.update({ wsConnected: connected });
    }

    setMaintenanceMode(maintenance: boolean): void {
        this.update({ maintenanceMode: maintenance });
    }

    setSelectedInstance(instanceId: string | null): void {
        this.update({ selectedInstanceId: instanceId });
    }

    setSelectedCycle(cycleId: string | null): void {
        this.update({ selectedCycleId: cycleId });
    }

    /** Update the last-seen event sequence for an instance. */
    setLastEventSeq(instanceId: string, seq: number): void {
        const seqMap = new Map(this.state.lastEventSeq);
        seqMap.set(instanceId, seq);
        this.update({ lastEventSeq: seqMap });
    }

    /** Set the full instance list (from the list endpoint). */
    setInstanceList(instances: Instance[]): void {
        this.update({ instanceList: instances });
    }

    /** Set the full view model for a specific instance. */
    setInstanceViewModel(id: string, vm: InstanceViewModel): void {
        const instances = new Map(this.state.instances);
        instances.set(id, vm);
        this.update({ instances });
    }

    // ── Data fetching ────────────────────────────────────────────

    /** Fetch the instance list from the API and update store. */
    async fetchInstances(): Promise<void> {
        this.setLoading(true);
        this.setError(null);
        try {
            const resp = await api.listInstances();
            this.setInstanceList(resp.data);
        } catch (e) {
            this.setError(e instanceof Error ? e.message : String(e));
        } finally {
            this.setLoading(false);
        }
    }

    /** Fetch full detail for an instance (cycles, tasks, budget). */
    async fetchInstanceDetail(instanceId: string): Promise<void> {
        this.setLoading(true);
        this.setError(null);
        try {
            const [instance, cyclesResp, tasksResp, budgetsResp] = await Promise.all([
                api.getInstance(instanceId),
                api.listCycles(instanceId),
                api.listTasks(instanceId),
                api.listBudgets(instanceId),
            ]);

            const totalSpentCents = budgetsResp.data.reduce(
                (sum: number, entry: BudgetEntry) => sum + Math.abs(entry.amount_cents),
                0,
            );

            this.setInstanceViewModel(instanceId, {
                instance,
                cycles: cyclesResp.data,
                tasks: tasksResp.data,
                budgetEntries: budgetsResp.data,
                totalSpentCents,
            });
        } catch (e) {
            this.setError(e instanceof Error ? e.message : String(e));
        } finally {
            this.setLoading(false);
        }
    }

    /**
     * Invalidate and re-fetch data for an instance.
     * Debounced — coalesces rapid WS events within a 500ms window.
     */
    async invalidate(instanceId: string): Promise<void> {
        const existing = this.invalidateTimers.get(instanceId);
        if (existing) clearTimeout(existing);

        const timer = setTimeout(async () => {
            this.invalidateTimers.delete(instanceId);
            if (this.state.selectedInstanceId === instanceId) {
                await this.fetchInstanceDetail(instanceId);
            }
            await this.fetchInstances();
        }, 500);

        this.invalidateTimers.set(instanceId, timer);
    }

    /** Immediate invalidate (bypass debounce), for user-initiated actions. */
    async invalidateNow(instanceId: string): Promise<void> {
        const existing = this.invalidateTimers.get(instanceId);
        if (existing) {
            clearTimeout(existing);
            this.invalidateTimers.delete(instanceId);
        }
        if (this.state.selectedInstanceId === instanceId) {
            await this.fetchInstanceDetail(instanceId);
        }
        await this.fetchInstances();
    }
}

// ── Singleton ────────────────────────────────────────────────────

export const store = new Store();
