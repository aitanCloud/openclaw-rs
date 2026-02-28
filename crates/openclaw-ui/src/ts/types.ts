// ── API response types (mirrors Rust backend models) ─────────────

/** Cursor-based pagination wrapper returned by all list endpoints. */
export interface PaginatedResponse<T> {
    data: T[];
    pagination: PaginationInfo;
}

export interface PaginationInfo {
    next_cursor: string | null;
    has_more: boolean;
}

/** Instance row from GET /api/v1/instances. */
export interface Instance {
    id: string;
    project_id: string;
    name: string;
    state: InstanceState;
    block_reason: string | null;
    started_at: string;
    last_heartbeat: string;
}

export type InstanceState =
    | 'provisioning'
    | 'active'
    | 'blocked'
    | 'suspended'
    | 'provisioning_failed';

/** Cycle row from GET /api/v1/instances/:id/cycles. */
export interface Cycle {
    id: string;
    instance_id: string;
    state: CycleState;
    prompt: string;
    plan: unknown | null;
    block_reason: string | null;
    failure_reason: string | null;
    cancel_reason: string | null;
    created_at: string;
    updated_at: string;
}

export type CycleState =
    | 'created'
    | 'planning'
    | 'plan_ready'
    | 'approved'
    | 'running'
    | 'blocked'
    | 'completing'
    | 'completed'
    | 'failed'
    | 'cancelled';

/** Task row from GET /api/v1/instances/:id/tasks. */
export interface Task {
    id: string;
    cycle_id: string;
    instance_id: string;
    task_key: string;
    phase: string;
    ordinal: number;
    state: TaskState;
    title: string;
    description: string;
    current_attempt: number;
    max_retries: number;
    failure_reason: string | null;
    cancel_reason: string | null;
    skip_reason: string | null;
    created_at: string;
    updated_at: string;
}

export type TaskState =
    | 'scheduled'
    | 'active'
    | 'verifying'
    | 'passed'
    | 'failed'
    | 'cancelled'
    | 'skipped';

/** Run row from GET /api/v1/instances/:id/runs/:run_id. */
export interface Run {
    id: string;
    task_id: string;
    instance_id: string;
    run_number: number;
    state: string;
    worker_session_id: string;
    exit_code: number | null;
    cost_cents: number;
    failure_category: string | null;
    cancel_reason: string | null;
    abandon_reason: string | null;
    started_at: string;
    finished_at: string | null;
}

/** Budget ledger entry from GET /api/v1/instances/:id/budgets. */
export interface BudgetEntry {
    id: string;
    instance_id: string;
    event_type: string;
    amount_cents: number;
    balance_after: number;
    created_at: string;
}

/** Event row from GET /api/v1/instances/:id/events. */
export interface EventRow {
    event_id: string;
    instance_id: string;
    seq: number;
    event_type: string;
    event_version: number;
    payload: unknown;
    idempotency_key: string | null;
    correlation_id: string | null;
    causation_id: string | null;
    occurred_at: string;
    recorded_at: string;
}

// ── WebSocket protocol types ─────────────────────────────────────

/** Messages the client sends to the server. */
export type ClientWsMessage =
    | { type: 'auth'; token: string }
    | { type: 'subscribe'; since_seq: number; event_types?: string[] };

/** Messages the server sends to the client. */
export type ServerWsMessage =
    | { type: 'event'; seq: number; event: EventRow }
    | { type: 'backfill_complete'; last_sent_seq: number; head_seq: number }
    | { type: 'heartbeat'; ts: string }
    | { type: 'reconnect' }
    | { type: 'error'; message: string };

// ── Request bodies ───────────────────────────────────────────────

export interface CreateInstanceRequest {
    name: string;
    project_id: string;
}

export interface CreateCycleRequest {
    prompt: string;
}

export interface ApprovePlanRequest {
    approved_by: string;
}

export interface MergeCycleRequest {
    task_id: string;
    run_id: string;
}

// ── App-level state ──────────────────────────────────────────────

/** View model for an instance (enriched with cycles/tasks/budget). */
export interface InstanceViewModel {
    instance: Instance;
    cycles: Cycle[];
    tasks: Task[];
    budgetEntries: BudgetEntry[];
    totalSpentCents: number;
}

/** Cycle progress summary for dashboard display. */
export interface CycleProgress {
    cycle: Cycle;
    tasks: Task[];
    tasksPassed: number;
    tasksFailed: number;
    tasksRunning: number;
    tasksTotal: number;
}

/** Ordered cycle states for stepper display. */
export const CYCLE_STEPS: CycleState[] = [
    'created',
    'planning',
    'plan_ready',
    'approved',
    'running',
    'completing',
    'completed',
];

/** Terminal cycle states. */
export const TERMINAL_CYCLE_STATES: CycleState[] = [
    'completed',
    'failed',
    'cancelled',
];

/** Stepper labels for cycle states. */
export const CYCLE_STEP_LABELS: Record<CycleState, string> = {
    created: 'Created',
    planning: 'Planning',
    plan_ready: 'Review',
    approved: 'Approved',
    running: 'Running',
    blocked: 'Blocked',
    completing: 'Completing',
    completed: 'Completed',
    failed: 'Failed',
    cancelled: 'Cancelled',
};

/** Central application state. */
export interface AppState {
    instances: Map<string, InstanceViewModel>;
    instanceList: Instance[];
    selectedInstanceId: string | null;
    selectedCycleId: string | null;
    wsConnected: boolean;
    lastEventSeq: Map<string, number>;
    maintenanceMode: boolean;
    loading: boolean;
    error: string | null;
}

// ── Routing ──────────────────────────────────────────────────────

export type Route =
    | { view: 'instance-list' }
    | { view: 'instance-detail'; instanceId: string }
    | { view: 'cycle-detail'; instanceId: string; cycleId: string };

// ── Component interface ──────────────────────────────────────────

export interface Component {
    el: HTMLElement;
    update(state: AppState): void;
    destroy(): void;
}
