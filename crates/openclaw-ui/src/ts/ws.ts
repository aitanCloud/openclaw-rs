import type { ClientWsMessage, ServerWsMessage } from './types';
import { getToken } from './api';
import { store } from './store';

// ── Constants ────────────────────────────────────────────────────

const RECONNECT_BASE_MS = 1_000;
const RECONNECT_MAX_MS = 30_000;

// ── WebSocket Manager ────────────────────────────────────────────

/**
 * Manages a WebSocket connection for a single instance.
 * Handles first-message auth, subscribe with backfill, reconnect with backoff.
 */
export class WsManager {
    private ws: WebSocket | null = null;
    private instanceId: string;
    private reconnectAttempt = 0;
    private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    private destroyed = false;

    constructor(instanceId: string) {
        this.instanceId = instanceId;
    }

    /** Open the WebSocket connection. */
    connect(): void {
        if (this.destroyed) return;
        if (this.ws) return;

        const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
        const url = `${protocol}//${location.host}/api/v1/instances/${this.instanceId}/events/ws`;

        this.ws = new WebSocket(url);
        this.ws.onopen = () => this.onOpen();
        this.ws.onmessage = (e) => this.onMessage(e);
        this.ws.onclose = () => this.onClose();
        this.ws.onerror = () => {
            // onclose will fire after onerror, so we handle reconnect there.
        };
    }

    /** Permanently close the connection and stop reconnecting. */
    destroy(): void {
        this.destroyed = true;
        if (this.reconnectTimer !== null) {
            clearTimeout(this.reconnectTimer);
            this.reconnectTimer = null;
        }
        if (this.ws) {
            this.ws.onopen = null;
            this.ws.onmessage = null;
            this.ws.onclose = null;
            this.ws.onerror = null;
            this.ws.close();
            this.ws = null;
        }
        store.setWsConnected(false);
    }

    // ── Internal handlers ────────────────────────────────────────

    private onOpen(): void {
        // Phase 1: Send auth message
        const token = getToken();
        if (!token) {
            console.warn('[ws] no token available, closing');
            this.ws?.close();
            return;
        }

        const authMsg: ClientWsMessage = { type: 'auth', token };
        this.ws!.send(JSON.stringify(authMsg));

        // Phase 2: Send subscribe message with last known seq
        const sinceSeq = store.getState().lastEventSeq.get(this.instanceId) ?? 0;
        const subMsg: ClientWsMessage = { type: 'subscribe', since_seq: sinceSeq };
        this.ws!.send(JSON.stringify(subMsg));

        this.reconnectAttempt = 0;
        store.setWsConnected(true);
    }

    private onMessage(event: MessageEvent): void {
        let msg: ServerWsMessage;
        try {
            msg = JSON.parse(event.data as string);
        } catch {
            console.warn('[ws] failed to parse message:', event.data);
            return;
        }

        switch (msg.type) {
            case 'event':
                // Track the sequence number and invalidate the store
                store.setLastEventSeq(this.instanceId, msg.seq);
                store.invalidate(this.instanceId);
                break;

            case 'backfill_complete':
                store.setLastEventSeq(this.instanceId, msg.last_sent_seq);
                // Fetch latest data after backfill is done
                store.invalidate(this.instanceId);
                break;

            case 'heartbeat':
                // No-op: keeps the connection alive
                break;

            case 'reconnect':
                // Server requests reconnection (broadcast overflow)
                console.info('[ws] server requested reconnect');
                this.ws?.close();
                break;

            case 'error':
                console.error('[ws] server error:', msg.message);
                break;
        }
    }

    private onClose(): void {
        this.ws = null;
        store.setWsConnected(false);

        if (this.destroyed) return;

        // Exponential backoff reconnect
        const delay = Math.min(
            RECONNECT_BASE_MS * Math.pow(2, this.reconnectAttempt),
            RECONNECT_MAX_MS,
        );
        this.reconnectAttempt++;
        console.info(`[ws] reconnecting in ${delay}ms (attempt ${this.reconnectAttempt})`);

        this.reconnectTimer = setTimeout(() => {
            this.reconnectTimer = null;
            this.connect();
        }, delay);
    }
}
