import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	BASE_RETRY_DELAY_MS,
	ConnectionManager,
	type ConnectionManagerDeps,
	type ConnectionState,
	connectionStore,
	HEARTBEAT_INTERVAL_MS,
	MAX_RECONNECT_ATTEMPTS,
	MAX_RETRY_DELAY_MS,
} from './connection';
import { client, torenStore } from './toren';

// ─── Mocks ───────────────────────────────────────────────────────────

vi.mock('./toren', async () => {
	const svelteStore = await import('svelte/store');

	const initialState = {
		connected: false,
		authenticated: false,
		connecting: false,
		error: null,
		sessionToken: null,
		shipUrl: 'http://localhost:8787',
		ancillaries: [],
		assignments: [],
		messages: [],
		segments: [],
		segmentRoots: [],
		selectedSegment: null,
		selectedAncillary: null,
		loadingSegments: false,
		loadingAssignments: false,
	};

	const { subscribe, set, update } = svelteStore.writable(initialState);

	const store = {
		subscribe,
		set,
		update,
		get: () => svelteStore.get({ subscribe }),
		reset: () => set({ ...initialState }),
		loadSegments: vi.fn().mockResolvedValue(undefined),
		loadAssignments: vi.fn().mockResolvedValue(undefined),
		loadAncillaries: vi.fn().mockResolvedValue(undefined),
		selectSegment: vi.fn(),
	};

	return {
		torenStore: store,
		client: {
			connect: vi.fn().mockResolvedValue(undefined),
			disconnect: vi.fn(),
			authenticate: vi.fn().mockResolvedValue(undefined),
			isConnected: vi.fn().mockReturnValue(false),
		},
		isConnected: svelteStore.derived(store, ($s: typeof initialState) => $s.connected),
		isAuthenticated: svelteStore.derived(store, ($s: typeof initialState) => $s.authenticated),
	};
});

// ─── Helpers ─────────────────────────────────────────────────────────

function createMockStorage(initial: Record<string, string> = {}): ConnectionManagerDeps['storage'] {
	const data = new Map(Object.entries(initial));
	return {
		getItem: vi.fn((key: string) => data.get(key) ?? null),
		setItem: vi.fn((key: string, value: string) => {
			data.set(key, value);
		}),
		removeItem: vi.fn((key: string) => {
			data.delete(key);
		}),
	};
}

function createTestDeps(overrides?: Partial<ConnectionManagerDeps>): ConnectionManagerDeps {
	return {
		createWebSocket: vi.fn(),
		fetch: vi.fn().mockResolvedValue({ ok: true }),
		storage: createMockStorage(),
		setTimeout: vi.fn().mockReturnValue(1),
		clearTimeout: vi.fn(),
		addVisibilityListener: vi.fn().mockReturnValue(() => {}),
		...overrides,
	};
}

function phase(): string {
	return get(connectionStore).phase;
}

function state(): ConnectionState {
	return get(connectionStore);
}

// ─── Tests ───────────────────────────────────────────────────────────

describe('ConnectionManager', () => {
	let mgr: ConnectionManager;

	beforeEach(() => {
		// Reset stores
		connectionStore.set({
			phase: 'idle',
			wasConnected: false,
			attempt: 0,
			lastError: null,
		});
		torenStore.reset();
		vi.clearAllMocks();
	});

	afterEach(() => {
		mgr?.destroy();
	});

	// ── Test 1: Auto-connect on init ───────────────────────────────

	it('auto-connects when stored credentials exist', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		// client.connect resolves → phase becomes authenticating
		// client.authenticate resolves → phase becomes connected
		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const onConnected = vi.fn().mockResolvedValue(undefined);
		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = onConnected;
		mgr.init();

		// Let promises resolve
		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(client.connect).toHaveBeenCalledWith('http://localhost:8787');
		expect(client.authenticate).toHaveBeenCalledWith('tok-123');
		expect(onConnected).toHaveBeenCalled();
		expect(state().wasConnected).toBe(true);
		expect(state().attempt).toBe(0);
	});

	// ── Test 2: No auto-connect without credentials ────────────────

	it('stays idle when no stored credentials', () => {
		const deps = createTestDeps();
		mgr = new ConnectionManager(deps);
		mgr.init();

		expect(phase()).toBe('idle');
		expect(client.connect).not.toHaveBeenCalled();
	});

	// ── Test 3: Auth failure clears credentials ────────────────────

	it('clears credentials and returns to idle on auth failure', async () => {
		const storage = createMockStorage({
			toren_session_token: 'bad-tok',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockResolvedValue(undefined);
		vi.mocked(client.authenticate).mockRejectedValue(new Error('Auth failed: Invalid token'));

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('idle');
		});

		expect(storage.removeItem).toHaveBeenCalledWith('toren_session_token');
		expect(storage.removeItem).toHaveBeenCalledWith('toren_ship_url');
		expect(state().lastError).toContain('Auth failed');
	});

	// ── Test 4: Reconnect on WS close ──────────────────────────────

	it('reconnects when notifyDisconnect is called', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		let connectCount = 0;
		vi.mocked(client.connect).mockImplementation(async () => {
			connectCount++;
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(connectCount).toBe(1);

		// Simulate WS close detected by the root layout
		mgr.notifyDisconnect();

		await vi.waitFor(() => {
			expect(connectCount).toBeGreaterThanOrEqual(2);
		});

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});
	});

	// ── Test 5: Reconnect on heartbeat failure ─────────────────────

	it('triggers reconnect when health check fails', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		// Capture the setTimeout callback for the heartbeat
		const timeoutCallbacks: Array<{ fn: () => void; ms: number }> = [];
		const mockSetTimeout = vi.fn((fn: () => void, ms: number) => {
			const id = timeoutCallbacks.length;
			timeoutCallbacks.push({ fn, ms });
			return id as unknown as ReturnType<typeof setTimeout>;
		});

		let fetchCallCount = 0;
		const mockFetch = vi.fn(async () => {
			fetchCallCount++;
			if (fetchCallCount === 1) {
				// First health check fails
				throw new Error('Network error');
			}
			return { ok: true } as Response;
		});

		const deps = createTestDeps({
			storage,
			setTimeout: mockSetTimeout,
			fetch: mockFetch,
		});
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		// Find and fire the heartbeat timeout
		const heartbeatEntry = timeoutCallbacks.find((e) => e.ms === HEARTBEAT_INTERVAL_MS);
		expect(heartbeatEntry).toBeDefined();
		heartbeatEntry?.fn();

		await vi.waitFor(() => {
			// Should have detected failure and started reconnecting
			expect(client.disconnect).toHaveBeenCalled();
		});
	});

	// ── Test 6: Exponential backoff ────────────────────────────────

	it('increases retry delay exponentially capped at MAX_RETRY_DELAY_MS', () => {
		expect(ConnectionManager.backoffDelay(1)).toBe(BASE_RETRY_DELAY_MS); // 1s
		expect(ConnectionManager.backoffDelay(2)).toBe(BASE_RETRY_DELAY_MS * 2); // 2s
		expect(ConnectionManager.backoffDelay(3)).toBe(BASE_RETRY_DELAY_MS * 4); // 4s
		expect(ConnectionManager.backoffDelay(4)).toBe(BASE_RETRY_DELAY_MS * 8); // 8s
		expect(ConnectionManager.backoffDelay(5)).toBe(BASE_RETRY_DELAY_MS * 16); // 16s
		expect(ConnectionManager.backoffDelay(6)).toBe(MAX_RETRY_DELAY_MS); // 30s (capped)
		expect(ConnectionManager.backoffDelay(10)).toBe(MAX_RETRY_DELAY_MS); // still 30s
	});

	// ── Test 7: Backoff resets on success ──────────────────────────

	it('resets attempt counter to 0 after successful reconnect', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);

		// Simulate that we already had some failed attempts
		connectionStore.update((s) => ({ ...s, attempt: 5 }));

		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(state().attempt).toBe(0);
	});

	// ── Test 8: Max retries → disconnected ─────────────────────────

	it('transitions to disconnected after max reconnect attempts', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockRejectedValue(new Error('Connection refused'));

		// Start near max attempts
		connectionStore.update((s) => ({
			...s,
			attempt: MAX_RECONNECT_ATTEMPTS - 1,
		}));

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('disconnected');
		});

		expect(state().attempt).toBe(MAX_RECONNECT_ATTEMPTS);
		expect(state().lastError).toBe('Connection refused');

		// Should NOT have scheduled a reconnect
		// (setTimeout is called for heartbeat/reconnect, but at this point no reconnect should be scheduled)
	});

	// ── Test 9: Visibility change triggers health check ────────────

	it('fires immediate health check on tab re-focus', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		let visibilityCallback: (() => void) | null = null;
		const mockFetch = vi.fn().mockResolvedValue({ ok: true });

		const deps = createTestDeps({
			storage,
			fetch: mockFetch,
			addVisibilityListener: vi.fn((fn) => {
				visibilityCallback = fn;
				return () => {
					visibilityCallback = null;
				};
			}),
		});

		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		// Reset fetch call count
		mockFetch.mockClear();

		// Simulate tab becoming visible
		expect(visibilityCallback).not.toBeNull();
		(visibilityCallback as unknown as () => void)();

		// Should have called fetch for health check
		await vi.waitFor(() => {
			expect(mockFetch).toHaveBeenCalledWith(
				'http://localhost:8787/health',
				expect.objectContaining({ signal: expect.any(AbortSignal) }),
			);
		});
	});

	// ── Test 10: Ancillary WS gates on auth ────────────────────────
	// This test validates the design principle that ancillary WS should
	// only connect when main connection is authenticated. The actual
	// gating is in the Svelte components, but we verify the store state
	// that components would react to.

	it('provides correct state for ancillary WS gating', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, authenticated: true }));
		});

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);

		// Before init: not connected, not authenticated
		expect(phase()).toBe('idle');
		expect(get(torenStore).authenticated).toBe(false);

		mgr.init();

		// During connect: phase is connecting, not yet authenticated
		// Components should NOT open ancillary WS here
		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
			expect(get(torenStore).authenticated).toBe(true);
		});

		// Now components CAN open ancillary WS

		// Simulate disconnect via notifyDisconnect (as root layout would)
		mgr.notifyDisconnect();

		// Phase should leave 'connected' (goes to 'disconnected' then reconnects)
		// The notifyDisconnect sets phase to 'disconnected' synchronously
		// before the async reconnect starts
		expect(get(torenStore).authenticated).toBe(false);
		// During the reconnect cycle, phase will be 'disconnected' or 'connecting'
		// Components should close ancillary WS when they see authenticated=false
	});

	// ── Test 11: Full reconnect cycle reloads data ─────────────────

	it('calls onConnected callback after successful reconnect', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		let connectCount = 0;
		vi.mocked(client.connect).mockImplementation(async () => {
			connectCount++;
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const onConnected = vi.fn().mockResolvedValue(undefined);
		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = onConnected;
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(onConnected).toHaveBeenCalledTimes(1);

		// Simulate disconnect via notifyDisconnect (as root layout would call)
		mgr.notifyDisconnect();

		// Wait for reconnect
		await vi.waitFor(() => {
			expect(connectCount).toBeGreaterThanOrEqual(2);
		});

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		// onConnected should be called again after reconnect
		expect(onConnected).toHaveBeenCalledTimes(2);
	});

	// ── Additional: connectAfterPair stores credentials ────────────

	it('stores credentials and connects after pairing', async () => {
		const storage = createMockStorage();

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.init(); // No stored creds → stays idle

		expect(phase()).toBe('idle');

		// User completes pairing
		mgr.connectAfterPair('http://myhost:9000', 'new-token');

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(storage.setItem).toHaveBeenCalledWith('toren_session_token', 'new-token');
		expect(storage.setItem).toHaveBeenCalledWith('toren_ship_url', 'http://myhost:9000');
		expect(client.connect).toHaveBeenCalledWith('http://myhost:9000');
		expect(client.authenticate).toHaveBeenCalledWith('new-token');
	});

	// ── Additional: retry resets attempts and reconnects ────────────

	it('retry() resets attempt counter and reconnects', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const deps = createTestDeps({ storage });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);

		// Start in disconnected state with high attempt count
		connectionStore.update((s) => ({
			...s,
			phase: 'disconnected',
			attempt: MAX_RECONNECT_ATTEMPTS,
		}));

		mgr.retry();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		expect(state().attempt).toBe(0);
	});

	// ── Test: Heartbeat triggers onHeartbeat callback ───────────────

	it('calls onHeartbeat after successful health check', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		// Capture the setTimeout callback for the heartbeat
		const timeoutCallbacks: Array<{ fn: () => void; ms: number }> = [];
		const mockSetTimeout = vi.fn((fn: () => void, ms: number) => {
			const id = timeoutCallbacks.length;
			timeoutCallbacks.push({ fn, ms });
			return id as unknown as ReturnType<typeof setTimeout>;
		});

		const mockFetch = vi.fn().mockResolvedValue({ ok: true } as Response);
		const onHeartbeat = vi.fn().mockResolvedValue(undefined);

		const deps = createTestDeps({
			storage,
			setTimeout: mockSetTimeout,
			fetch: mockFetch,
		});
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.onHeartbeat = onHeartbeat;
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		// Find and fire the heartbeat timeout
		const heartbeatEntry = timeoutCallbacks.find((e) => e.ms === HEARTBEAT_INTERVAL_MS);
		expect(heartbeatEntry).toBeDefined();
		heartbeatEntry?.fn();

		await vi.waitFor(() => {
			expect(onHeartbeat).toHaveBeenCalledTimes(1);
		});
	});

	// ── Additional: destroy stops everything ───────────────────────

	it('destroy() stops heartbeat and reconnect timers', async () => {
		const storage = createMockStorage({
			toren_session_token: 'tok-123',
			toren_ship_url: 'http://localhost:8787',
		});

		vi.mocked(client.connect).mockImplementation(async () => {
			torenStore.update((s) => ({ ...s, connected: true, connecting: false }));
		});
		vi.mocked(client.authenticate).mockResolvedValue(undefined);

		const mockClearTimeout = vi.fn();
		const deps = createTestDeps({ storage, clearTimeout: mockClearTimeout });
		mgr = new ConnectionManager(deps);
		mgr.onConnected = vi.fn().mockResolvedValue(undefined);
		mgr.init();

		await vi.waitFor(() => {
			expect(phase()).toBe('connected');
		});

		mgr.destroy();

		// clearTimeout should have been called (for heartbeat cleanup)
		expect(mockClearTimeout).toHaveBeenCalled();
	});
});
