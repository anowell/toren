import { writable, derived, get } from 'svelte/store';
import { client, torenStore } from './toren';

export type ConnectionPhase = 'idle' | 'connecting' | 'authenticating' | 'connected' | 'disconnected';

export interface ConnectionState {
	phase: ConnectionPhase;
	/** Whether we had a successful connection before (used to distinguish "reconnecting" from "first connect") */
	wasConnected: boolean;
	/** Current reconnect attempt number (0 when connected) */
	attempt: number;
	/** Error from the last failed attempt */
	lastError: string | null;
}

const INITIAL_STATE: ConnectionState = {
	phase: 'idle',
	wasConnected: false,
	attempt: 0,
	lastError: null,
};

export const connectionStore = writable<ConnectionState>({ ...INITIAL_STATE });
export const connectionPhase = derived(connectionStore, ($c) => $c.phase);

export interface ConnectionManagerDeps {
	/** Factory to create a WebSocket (injectable for testing) */
	createWebSocket: (url: string) => WebSocket;
	/** fetch function (injectable for testing) */
	fetch: (url: string, init?: RequestInit) => Promise<Response>;
	/** localStorage (injectable for testing) */
	storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
	/** setTimeout (injectable for testing) */
	setTimeout: (fn: () => void, ms: number) => ReturnType<typeof globalThis.setTimeout>;
	/** clearTimeout (injectable for testing) */
	clearTimeout: (id: ReturnType<typeof globalThis.setTimeout>) => void;
	/** addEventListener for document visibility changes */
	addVisibilityListener: (fn: () => void) => () => void;
}

const defaultDeps: ConnectionManagerDeps = {
	createWebSocket: (url) => new WebSocket(url),
	fetch: globalThis.fetch?.bind(globalThis),
	storage: typeof localStorage !== 'undefined' ? localStorage : { getItem: () => null, setItem: () => {}, removeItem: () => {} },
	setTimeout: globalThis.setTimeout.bind(globalThis),
	clearTimeout: globalThis.clearTimeout.bind(globalThis),
	addVisibilityListener: (fn) => {
		const handler = () => {
			if (document.visibilityState === 'visible') fn();
		};
		document.addEventListener('visibilitychange', handler);
		return () => document.removeEventListener('visibilitychange', handler);
	},
};

export const HEARTBEAT_INTERVAL_MS = 15_000;
export const BASE_RETRY_DELAY_MS = 1_000;
export const MAX_RETRY_DELAY_MS = 30_000;
export const MAX_RECONNECT_ATTEMPTS = 10;

export class ConnectionManager {
	private deps: ConnectionManagerDeps;
	private heartbeatTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
	private reconnectTimer: ReturnType<typeof globalThis.setTimeout> | null = null;
	private removeVisibilityListener: (() => void) | null = null;
	private destroyed = false;
	private currentShipUrl: string | null = null;
	private currentToken: string | null = null;

	/** Callback invoked after a successful connect+auth cycle (used to reload data) */
	onConnected: (() => Promise<void>) | null = null;

	/** Callback invoked after each successful heartbeat (used to refresh data) */
	onHeartbeat: (() => Promise<void>) | null = null;

	constructor(deps?: Partial<ConnectionManagerDeps>) {
		this.deps = { ...defaultDeps, ...deps };
	}

	/** Start the manager. If stored credentials exist, auto-connect. */
	init(): void {
		const token = this.deps.storage.getItem('toren_session_token');
		const url = this.deps.storage.getItem('toren_ship_url');

		// Listen for visibility changes
		this.removeVisibilityListener = this.deps.addVisibilityListener(() => {
			this.onVisibilityChange();
		});

		if (token && url) {
			this.connectFull(url, token);
		}
		// Otherwise stay in idle — PairingModal will call connectAfterPair
	}

	/** Called after a successful pairing to kick off the first connection */
	connectAfterPair(shipUrl: string, sessionToken: string): void {
		this.deps.storage.setItem('toren_session_token', sessionToken);
		this.deps.storage.setItem('toren_ship_url', shipUrl);
		this.connectFull(shipUrl, sessionToken);
	}

	/**
	 * Notify the manager that the WebSocket disconnected.
	 * Called from the root layout's torenStore subscription when connected goes false.
	 * Triggers the reconnect cycle if we were in 'connected' phase.
	 */
	notifyDisconnect(): void {
		const state = get(connectionStore);
		if (state.phase !== 'connected') return;

		this.stopHeartbeat();
		connectionStore.update((s) => ({ ...s, phase: 'disconnected', lastError: 'WebSocket closed' }));
		torenStore.update((s) => ({ ...s, connected: false, authenticated: false, connecting: false }));

		if (this.currentShipUrl && this.currentToken) {
			this.connectFull(this.currentShipUrl, this.currentToken);
		}
	}

	/** Full connect cycle: WS connect → authenticate → load data */
	private async connectFull(shipUrl: string, sessionToken: string): Promise<void> {
		if (this.destroyed) return;

		const state = get(connectionStore);
		// Don't start if already connecting/authenticating
		if (state.phase === 'connecting' || state.phase === 'authenticating') return;

		this.clearReconnectTimer();
		this.currentShipUrl = shipUrl;
		this.currentToken = sessionToken;

		connectionStore.update((s) => ({
			...s,
			phase: 'connecting',
			lastError: null,
		}));

		// Update toren store for backwards compat
		torenStore.update((s) => ({ ...s, shipUrl, connecting: true }));

		try {
			// Step 1: Connect WebSocket
			await client.connect(shipUrl);

			if (this.destroyed) return;
			connectionStore.update((s) => ({ ...s, phase: 'authenticating' }));

			// Step 2: Authenticate
			await client.authenticate(sessionToken);

			if (this.destroyed) return;
			connectionStore.update((s) => ({
				...s,
				phase: 'connected',
				wasConnected: true,
				attempt: 0,
				lastError: null,
			}));

			// Step 3: Load data
			if (this.onConnected) {
				await this.onConnected();
			}

			// Step 4: Start heartbeat
			this.startHeartbeat(shipUrl);
		} catch (err) {
			if (this.destroyed) return;
			const message = err instanceof Error ? err.message : 'Connection failed';
			this.handleConnectFailure(shipUrl, sessionToken, message);
		}
	}

	private handleConnectFailure(shipUrl: string, sessionToken: string, message: string): void {
		const state = get(connectionStore);

		// Auth failure: clear credentials and go to idle
		if (state.phase === 'authenticating' || message.includes('Auth failed') || message.includes('Invalid token')) {
			this.clearCredentials();
			connectionStore.update((s) => ({
				...s,
				phase: 'idle',
				attempt: 0,
				lastError: message,
			}));
			torenStore.update((s) => ({
				...s,
				connected: false,
				authenticated: false,
				connecting: false,
				error: message,
			}));
			return;
		}

		// Connection failure: retry with backoff
		const attempt = state.attempt + 1;
		if (attempt >= MAX_RECONNECT_ATTEMPTS) {
			connectionStore.update((s) => ({
				...s,
				phase: 'disconnected',
				attempt,
				lastError: message,
			}));
			torenStore.update((s) => ({
				...s,
				connected: false,
				authenticated: false,
				connecting: false,
				error: message,
			}));
			return;
		}

		connectionStore.update((s) => ({
			...s,
			phase: 'disconnected',
			attempt,
			lastError: message,
		}));
		torenStore.update((s) => ({
			...s,
			connected: false,
			authenticated: false,
			connecting: false,
		}));

		this.scheduleReconnect(shipUrl, sessionToken, attempt);
	}

	private scheduleReconnect(shipUrl: string, sessionToken: string, attempt: number): void {
		this.clearReconnectTimer();
		const delay = Math.min(BASE_RETRY_DELAY_MS * Math.pow(2, attempt - 1), MAX_RETRY_DELAY_MS);

		this.reconnectTimer = this.deps.setTimeout(() => {
			this.reconnectTimer = null;
			// Reset phase so connectFull doesn't bail
			connectionStore.update((s) => ({ ...s, phase: 'disconnected' }));
			this.connectFull(shipUrl, sessionToken);
		}, delay);
	}

	private startHeartbeat(shipUrl: string): void {
		this.stopHeartbeat();
		this.heartbeatTimer = this.deps.setTimeout(() => this.heartbeatTick(shipUrl), HEARTBEAT_INTERVAL_MS);
	}

	private async heartbeatTick(shipUrl: string): Promise<void> {
		if (this.destroyed) return;

		const state = get(connectionStore);
		if (state.phase !== 'connected') return;

		try {
			const resp = await this.deps.fetch(`${shipUrl}/health`, { signal: AbortSignal.timeout(5000) });
			if (!resp.ok) throw new Error(`Health check returned ${resp.status}`);

			if (this.destroyed) return;

			// Refresh data on successful heartbeat
			if (this.onHeartbeat) {
				await this.onHeartbeat();
			}

			if (this.destroyed) return;
			// Schedule next tick
			this.heartbeatTimer = this.deps.setTimeout(() => this.heartbeatTick(shipUrl), HEARTBEAT_INTERVAL_MS);
		} catch {
			if (this.destroyed) return;
			// Health check failed — trigger reconnect
			const token = this.deps.storage.getItem('toren_session_token');
			if (!token) return;

			client.disconnect();
			connectionStore.update((s) => ({ ...s, phase: 'disconnected', lastError: 'Health check failed' }));
			torenStore.update((s) => ({ ...s, connected: false, authenticated: false }));
			this.connectFull(shipUrl, token);
		}
	}

	private stopHeartbeat(): void {
		if (this.heartbeatTimer !== null) {
			this.deps.clearTimeout(this.heartbeatTimer);
			this.heartbeatTimer = null;
		}
	}

	/** Handle tab becoming visible — fire immediate health check */
	private onVisibilityChange(): void {
		const state = get(connectionStore);
		if (state.phase === 'connected') {
			const shipUrl = this.deps.storage.getItem('toren_ship_url');
			if (shipUrl) {
				// Fire an immediate heartbeat
				this.stopHeartbeat();
				this.heartbeatTick(shipUrl);
			}
		} else if (state.phase === 'disconnected') {
			// Try reconnecting
			const token = this.deps.storage.getItem('toren_session_token');
			const url = this.deps.storage.getItem('toren_ship_url');
			if (token && url) {
				this.connectFull(url, token);
			}
		}
	}

	private clearCredentials(): void {
		this.deps.storage.removeItem('toren_session_token');
		this.deps.storage.removeItem('toren_ship_url');
	}

	private clearReconnectTimer(): void {
		if (this.reconnectTimer !== null) {
			this.deps.clearTimeout(this.reconnectTimer);
			this.reconnectTimer = null;
		}
	}

	/** Call retry from UI (e.g. "Retry" button when disconnected) */
	retry(): void {
		const token = this.deps.storage.getItem('toren_session_token');
		const url = this.deps.storage.getItem('toren_ship_url');
		if (token && url) {
			connectionStore.update((s) => ({ ...s, phase: 'disconnected', attempt: 0 }));
			this.connectFull(url, token);
		}
	}

	/** Tear down — stop heartbeat, timers, listeners */
	destroy(): void {
		this.destroyed = true;
		this.stopHeartbeat();
		this.clearReconnectTimer();
		if (this.removeVisibilityListener) {
			this.removeVisibilityListener();
			this.removeVisibilityListener = null;
		}
	}

	/** Compute backoff delay for a given attempt (exported for testing) */
	static backoffDelay(attempt: number): number {
		return Math.min(BASE_RETRY_DELAY_MS * Math.pow(2, attempt - 1), MAX_RETRY_DELAY_MS);
	}
}
