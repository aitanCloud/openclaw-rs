import type {
    PaginatedResponse,
    Instance,
    Cycle,
    Task,
    Run,
    BudgetEntry,
    EventRow,
    CreateInstanceRequest,
    CreateCycleRequest,
    ApprovePlanRequest,
    MergeCycleRequest,
} from './types';

// ── Token storage ────────────────────────────────────────────────

const TOKEN_KEY = 'openclaw_token';

export function getToken(): string | null {
    return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
    localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
    localStorage.removeItem(TOKEN_KEY);
}

// ── API error ────────────────────────────────────────────────────

export class ApiError extends Error {
    constructor(
        public status: number,
        public statusText: string,
        public body: string,
    ) {
        super(`API ${status}: ${statusText}`);
        this.name = 'ApiError';
    }
}

// ── Fetch wrapper ────────────────────────────────────────────────

async function request<T>(
    method: string,
    path: string,
    body?: unknown,
): Promise<T> {
    const token = getToken();
    const headers: Record<string, string> = {
        'Content-Type': 'application/json',
    };
    if (token) {
        headers['Authorization'] = `Bearer ${token}`;
    }

    const opts: RequestInit = { method, headers };
    if (body !== undefined) {
        opts.body = JSON.stringify(body);
    }

    const response = await fetch(path, opts);

    if (!response.ok) {
        const text = await response.text().catch(() => '');

        if (response.status === 401) {
            clearToken();
            // Dispatch event so the app can show a login prompt
            window.dispatchEvent(new CustomEvent('openclaw:auth-required'));
        }
        if (response.status === 503) {
            window.dispatchEvent(new CustomEvent('openclaw:maintenance'));
        }

        throw new ApiError(response.status, response.statusText, text);
    }

    return response.json() as Promise<T>;
}

// ── API client ───────────────────────────────────────────────────

export const api = {
    // ── Instances ────────────────────────────────────────────────

    listInstances(
        cursor?: string,
        limit?: number,
    ): Promise<PaginatedResponse<Instance>> {
        const params = new URLSearchParams();
        if (cursor) params.set('cursor', cursor);
        if (limit) params.set('limit', String(limit));
        const qs = params.toString();
        return request('GET', `/api/v1/instances${qs ? '?' + qs : ''}`);
    },

    getInstance(id: string): Promise<Instance> {
        return request('GET', `/api/v1/instances/${id}`);
    },

    createInstance(body: CreateInstanceRequest): Promise<{ instance_id: string; name: string; state: string }> {
        return request('POST', '/api/v1/instances', body);
    },

    // ── Cycles ───────────────────────────────────────────────────

    listCycles(
        instanceId: string,
        cursor?: string,
        limit?: number,
    ): Promise<PaginatedResponse<Cycle>> {
        const params = new URLSearchParams();
        if (cursor) params.set('cursor', cursor);
        if (limit) params.set('limit', String(limit));
        const qs = params.toString();
        return request('GET', `/api/v1/instances/${instanceId}/cycles${qs ? '?' + qs : ''}`);
    },

    getCycle(instanceId: string, cycleId: string): Promise<Cycle> {
        return request('GET', `/api/v1/instances/${instanceId}/cycles/${cycleId}`);
    },

    createCycle(instanceId: string, body: CreateCycleRequest): Promise<{ cycle_id: string; state: string }> {
        return request('POST', `/api/v1/instances/${instanceId}/cycles`, body);
    },

    approvePlan(
        instanceId: string,
        cycleId: string,
        body: ApprovePlanRequest,
    ): Promise<{ cycle_id: string; state: string }> {
        return request('POST', `/api/v1/instances/${instanceId}/cycles/${cycleId}/approve`, body);
    },

    triggerMerge(
        instanceId: string,
        cycleId: string,
        body: MergeCycleRequest,
    ): Promise<{ cycle_id: string; status: string }> {
        return request('POST', `/api/v1/instances/${instanceId}/cycles/${cycleId}/merge`, body);
    },

    // ── Tasks ────────────────────────────────────────────────────

    listTasks(
        instanceId: string,
        cycleId?: string,
        cursor?: string,
        limit?: number,
    ): Promise<PaginatedResponse<Task>> {
        const params = new URLSearchParams();
        if (cycleId) params.set('cycle_id', cycleId);
        if (cursor) params.set('cursor', cursor);
        if (limit) params.set('limit', String(limit));
        const qs = params.toString();
        return request('GET', `/api/v1/instances/${instanceId}/tasks${qs ? '?' + qs : ''}`);
    },

    // ── Runs ─────────────────────────────────────────────────────

    getRun(instanceId: string, runId: string): Promise<Run> {
        return request('GET', `/api/v1/instances/${instanceId}/runs/${runId}`);
    },

    // ── Budgets ──────────────────────────────────────────────────

    listBudgets(
        instanceId: string,
        cursor?: string,
        limit?: number,
    ): Promise<PaginatedResponse<BudgetEntry>> {
        const params = new URLSearchParams();
        if (cursor) params.set('cursor', cursor);
        if (limit) params.set('limit', String(limit));
        const qs = params.toString();
        return request('GET', `/api/v1/instances/${instanceId}/budgets${qs ? '?' + qs : ''}`);
    },

    // ── Events ───────────────────────────────────────────────────

    listEvents(
        instanceId: string,
        eventType?: string,
        cursor?: string,
        limit?: number,
    ): Promise<PaginatedResponse<EventRow>> {
        const params = new URLSearchParams();
        if (eventType) params.set('event_type', eventType);
        if (cursor) params.set('cursor', cursor);
        if (limit) params.set('limit', String(limit));
        const qs = params.toString();
        return request('GET', `/api/v1/instances/${instanceId}/events${qs ? '?' + qs : ''}`);
    },
};
