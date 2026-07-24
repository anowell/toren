import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { torenStore } from './toren';

describe('Toren Store', () => {
	beforeEach(() => {
		torenStore.reset();
	});

	it('should initialize with default state', () => {
		const state = get(torenStore);

		expect(state.connected).toBe(false);
		expect(state.authenticated).toBe(false);
		expect(state.connecting).toBe(false);
		expect(state.error).toBeNull();
		expect(state.sessionToken).toBeNull();
		expect(state.shipUrl).toBe('http://localhost:8787');
		expect(state.workspaces).toEqual([]);
		expect(state.messages).toEqual([]);
	});

	it('should update connection state', () => {
		torenStore.update((state) => ({
			...state,
			connected: true,
			connecting: false,
		}));

		const state = get(torenStore);
		expect(state.connected).toBe(true);
		expect(state.connecting).toBe(false);
	});

	it('should update authentication state', () => {
		const token = 'test-token-123';

		torenStore.update((state) => ({
			...state,
			authenticated: true,
			sessionToken: token,
		}));

		const state = get(torenStore);
		expect(state.authenticated).toBe(true);
		expect(state.sessionToken).toBe(token);
	});

	it('should add messages to the store', () => {
		const message = {
			id: 'msg-1',
			role: 'user' as const,
			content: 'Hello Toren',
			timestamp: new Date(),
		};

		torenStore.update((state) => ({
			...state,
			messages: [...state.messages, message],
		}));

		const state = get(torenStore);
		expect(state.messages).toHaveLength(1);
		expect(state.messages[0].content).toBe('Hello Toren');
		expect(state.messages[0].role).toBe('user');
	});

	it('should handle error state', () => {
		const errorMessage = 'Connection failed';

		torenStore.update((state) => ({
			...state,
			error: errorMessage,
		}));

		const state = get(torenStore);
		expect(state.error).toBe(errorMessage);
	});

	describe('startWorkspaceShell', () => {
		afterEach(() => {
			vi.unstubAllGlobals();
		});

		it('POSTs to the shell endpoint and returns the new window name', async () => {
			const fetchMock = vi.fn().mockResolvedValue({
				ok: true,
				json: async () => ({ success: true, session: 'toren/one', window: 'shell-2' }),
			});
			vi.stubGlobal('fetch', fetchMock);

			const window = await torenStore.startWorkspaceShell('http://ship.local', 'toren', 'one');

			expect(window).toBe('shell-2');
			expect(fetchMock).toHaveBeenCalledWith(
				'http://ship.local/api/workspaces/toren/one/shell',
				expect.objectContaining({ method: 'POST' }),
			);
		});

		it('throws with the server error message on failure', async () => {
			vi.stubGlobal(
				'fetch',
				vi.fn().mockResolvedValue({
					ok: false,
					json: async () => ({ error: 'no session' }),
				}),
			);

			await expect(
				torenStore.startWorkspaceShell('http://ship.local', 'toren', 'one'),
			).rejects.toThrow('no session');
		});
	});

	it('should reset to initial state', () => {
		// Modify state
		torenStore.update((state) => ({
			...state,
			connected: true,
			authenticated: true,
			sessionToken: 'test-token',
			messages: [
				{
					id: 'msg-1',
					role: 'user',
					content: 'Test',
					timestamp: new Date(),
				},
			],
		}));

		// Reset
		torenStore.reset();

		// Verify reset
		const state = get(torenStore);
		expect(state.connected).toBe(false);
		expect(state.authenticated).toBe(false);
		expect(state.sessionToken).toBeNull();
		expect(state.messages).toEqual([]);
	});
});
