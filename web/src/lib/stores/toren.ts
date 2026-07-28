import { derived, get, writable } from 'svelte/store';
import type {
	AgentInfo,
	AgentSession,
	AgentsResponse,
	CommandOutput,
	Segment,
	SessionInfo,
	StartWorkspaceRequest,
	TaskDisplayStatus,
	TaskView,
	WorkflowRequest,
	WorkflowResponse,
	WorkflowVerb,
	WorkspaceDisplayStatus,
	WorkspaceSessionsResponse,
	WorkspaceView,
	WsRequest,
	WsResponse,
} from '$lib/types/toren';

export interface TorenState {
	connected: boolean;
	authenticated: boolean;
	connecting: boolean;
	error: string | null;
	sessionToken: string | null;
	shipUrl: string;
	workspaces: WorkspaceView[];
	messages: ChatMessage[];
	segments: Segment[];
	segmentRoots: string[];
	selectedSegment: Segment | null;
	loadingSegments: boolean;
	loadingWorkspaces: boolean;
}

export interface ChatMessage {
	id: string;
	role: 'user' | 'assistant' | 'system';
	content: string;
	timestamp: Date;
	commandOutputs?: CommandOutput[];
}

class TorenClient {
	private ws: WebSocket | null = null;
	private _authHandler: ((message: WsResponse) => void) | null = null;

	async connect(shipUrl: string): Promise<void> {
		return new Promise((resolve, reject) => {
			const wsUrl = `${shipUrl.replace(/^http/, 'ws')}/ws`;

			try {
				this.ws = new WebSocket(wsUrl);

				this.ws.onopen = () => {
					console.log('Connected to Toren');
					torenStore.update((state) => ({
						...state,
						connected: true,
						connecting: false,
						error: null,
					}));
					resolve();
				};

				this.ws.onmessage = (event) => {
					try {
						const message: WsResponse = JSON.parse(event.data);
						this.handleMessage(message);
					} catch (error) {
						console.error('Failed to parse message:', error);
					}
				};

				this.ws.onerror = (error) => {
					console.error('WebSocket error:', error);
					torenStore.update((state) => ({
						...state,
						error: 'Connection error',
						connecting: false,
					}));
					reject(new Error('Connection error'));
				};

				this.ws.onclose = () => {
					console.log('Disconnected from Toren');
					torenStore.update((state) => ({
						...state,
						connected: false,
						authenticated: false,
					}));
					// Reconnect is handled by ConnectionManager via notifyDisconnect()
				};
			} catch (error) {
				torenStore.update((state) => ({
					...state,
					error: 'Failed to create WebSocket',
					connecting: false,
				}));
				reject(error);
			}
		});
	}

	disconnect(): void {
		if (this.ws) {
			this.ws.close();
			this.ws = null;
		}
	}

	async authenticate(token: string): Promise<void> {
		return new Promise((resolve, reject) => {
			const timeout = setTimeout(() => {
				reject(new Error('Authentication timeout'));
			}, 5000);

			const handler = (message: WsResponse) => {
				if (message.type === 'AuthSuccess') {
					clearTimeout(timeout);
					torenStore.update((state) => ({
						...state,
						authenticated: true,
						sessionToken: token,
						error: null,
					}));
					resolve();
				} else if (message.type === 'AuthFailure') {
					clearTimeout(timeout);
					torenStore.update((state) => ({
						...state,
						error: `Auth failed: ${message.reason}`,
					}));
					reject(new Error(message.reason));
				}
			};

			// Subscribe once to the next message
			const _unsubscribe = torenStore.subscribe((_state) => {
				// This is a hack - we should use proper event emitter
				// For now, messages are handled in handleMessage
			});

			this.send({ type: 'Auth', token });

			// Store handler for later
			this._authHandler = handler;
		});
	}

	private handleMessage(message: WsResponse): void {
		console.log('Received message:', message);

		// Handle auth responses
		if (this._authHandler) {
			this._authHandler(message);
			this._authHandler = null;
			return;
		}

		switch (message.type) {
			case 'CommandOutput':
				torenStore.update((state) => {
					const messages = [...state.messages];
					const lastMessage = messages[messages.length - 1];
					if (lastMessage && lastMessage.role === 'assistant') {
						if (!lastMessage.commandOutputs) {
							lastMessage.commandOutputs = [];
						}
						lastMessage.commandOutputs.push(message.output);
					}
					return { ...state, messages };
				});
				break;

			case 'Error':
				torenStore.update((state) => ({
					...state,
					error: message.message,
				}));
				break;

			case 'FileContent':
			case 'VcsStatus':
				// Handle other message types as needed
				break;
		}
	}

	private send(message: WsRequest): void {
		if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
			throw new Error('WebSocket not connected');
		}
		this.ws.send(JSON.stringify(message));
	}

	async sendCommand(command: string, args: string[], cwd?: string): Promise<void> {
		this.send({
			type: 'Command',
			request: { command, args, cwd },
		});
	}

	isConnected(): boolean {
		return this.ws !== null && this.ws.readyState === WebSocket.OPEN;
	}
}

// Create singleton instance
export const client = new TorenClient();

// Status mapping helpers

/** A workspace is "busy" when any of its sessions has a running pane. */
export function getWorkspaceDisplayStatus(ws: WorkspaceView): WorkspaceDisplayStatus {
	return ws.sets.sessions.some((s) => s.status === 'running') ? 'busy' : 'ready';
}

/** Strip a "source-" / "source:" prefix off a task id for compact display. */
export function stripTaskPrefix(id: string): string {
	const sep = id.search(/[-:]/);
	return sep >= 0 ? id.slice(sep + 1) : id;
}

/** Native provider task status is pass-through; normalize it to an icon bucket. */
export function getTaskDisplayStatus(task: TaskView): TaskDisplayStatus {
	const status = (task.status ?? '').toLowerCase();
	if (status.includes('clos') || status.includes('done') || status.includes('complete')) {
		return 'closed';
	}
	if (status.includes('progress') || status.includes('active')) {
		return 'in_progress';
	}
	if (status.includes('open') || status.includes('todo') || status.includes('backlog')) {
		return 'open';
	}
	// Unknown/absent: treat as in progress (the workspace exists, so work is underway).
	return 'in_progress';
}

/** Primary task for a workspace, if any (first collected task). */
export function primaryTask(ws: WorkspaceView): TaskView | null {
	return ws.sets.tasks[0] ?? null;
}

/**
 * Which window a bare attach lands on, mirroring the daemon's `default_window` logic:
 * the `agent` window if present, else the birth `shell`, else the first observable window.
 * Returns null when there are no windows.
 */
export function defaultWindowName(sessions: SessionInfo[]): string | null {
	if (sessions.length === 0) return null;
	const agent = sessions.find((s) => s.window === 'agent');
	if (agent) return agent.window;
	const shell = sessions.find((s) => s.window === 'shell');
	return (shell ?? sessions[0]).window;
}

// Create the store with a custom store that includes helper methods
function createTorenStore() {
	const initialState: TorenState = {
		connected: false,
		authenticated: false,
		connecting: false,
		error: null,
		sessionToken: null,
		shipUrl: 'http://localhost:8787',
		workspaces: [],
		messages: [],
		segments: [],
		segmentRoots: [],
		selectedSegment: null,
		loadingSegments: false,
		loadingWorkspaces: false,
	};

	const { subscribe, set, update } = writable(initialState);

	return {
		subscribe,
		set,
		update,
		get: () => get({ subscribe }),
		reset: () => set(initialState),
		async loadSegments(shipUrl: string) {
			update((state) => ({ ...state, loadingSegments: true }));
			try {
				const response = await fetch(`${shipUrl}/api/segments/list`);
				if (!response.ok) throw new Error('Failed to fetch segments');
				const data = await response.json();
				update((state) => ({
					...state,
					segments: data.segments ?? [],
					segmentRoots: data.roots ?? [],
					loadingSegments: false,
				}));
			} catch (error) {
				console.error('Failed to load segments:', error);
				update((state) => ({
					...state,
					loadingSegments: false,
					error: 'Failed to load segments',
				}));
			}
		},
		selectSegment(segment: Segment | null) {
			update((state) => ({ ...state, selectedSegment: segment }));
			if (segment) {
				localStorage.setItem('toren_selected_segment', JSON.stringify(segment));
			} else {
				localStorage.removeItem('toren_selected_segment');
			}
		},
		async createSegment(name: string, root: string, shipUrl: string) {
			try {
				const response = await fetch(`${shipUrl}/api/segments/create`, {
					method: 'POST',
					headers: { 'Content-Type': 'application/json' },
					body: JSON.stringify({ name, root }),
				});
				if (!response.ok) throw new Error('Failed to create segment');
				const data = await response.json();
				update((state) => ({
					...state,
					segments: [...state.segments, data.segment],
				}));
				return data.segment;
			} catch (error) {
				console.error('Failed to create segment:', error);
				throw error;
			}
		},
		async loadWorkspaces(shipUrl: string) {
			update((state) => ({ ...state, loadingWorkspaces: true }));
			try {
				const response = await fetch(`${shipUrl}/api/workspaces`);
				if (!response.ok) throw new Error('Failed to fetch workspaces');
				const data = await response.json();
				update((state) => ({
					...state,
					workspaces: data.workspaces ?? [],
					loadingWorkspaces: false,
				}));
			} catch (error) {
				console.error('Failed to load workspaces:', error);
				update((state) => ({
					...state,
					loadingWorkspaces: false,
				}));
			}
		},
		/** Refresh a single workspace and merge it into local state. */
		async refreshWorkspace(
			shipUrl: string,
			segment: string,
			name: string,
		): Promise<WorkspaceView | null> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}`);
			if (!response.ok) return null;
			const data = await response.json();
			const workspace: WorkspaceView = data.workspace;
			update((state) => ({
				...state,
				workspaces: state.workspaces.map((w) =>
					w.segment === workspace.segment && w.name === workspace.name ? workspace : w,
				),
			}));
			return workspace;
		},
		async startWorkspace(
			shipUrl: string,
			segment: string,
			name: string,
			request: StartWorkspaceRequest = {},
		): Promise<string> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/start`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify(request),
			});
			if (!response.ok) {
				const data = await response.json().catch(() => ({}));
				throw new Error(data.error || 'Failed to start workspace');
			}
			const data = await response.json();
			return data.session;
		},
		/** Open a new shell window in the workspace's session; resolves to the new window name. */
		async startWorkspaceShell(shipUrl: string, segment: string, name: string): Promise<string> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/shell`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
			});
			if (!response.ok) {
				const data = await response.json().catch(() => ({}));
				throw new Error(data.error || 'Failed to open shell');
			}
			const data = await response.json();
			return data.window;
		},
		/**
		 * The agents the daemon can actually start, so "New agent" names them one by one.
		 *
		 * The daemon reports every agent it has a plugin for; the ones whose binary is not on its
		 * host are dropped here, because offering to start them is offering a failure.
		 */
		async loadAgents(shipUrl: string): Promise<AgentInfo[]> {
			const response = await fetch(`${shipUrl}/api/agents`);
			if (!response.ok) throw new Error('Failed to load agents');
			const data: AgentsResponse = await response.json();
			return (data.agents ?? []).filter((agent) => agent.installed);
		},
		/**
		 * The workspace's recorded agent sessions, newest first.
		 *
		 * Its own endpoint rather than the workspace view's copy: picking a session to resume is
		 * not worth the task and PR round trips that view makes.
		 */
		async loadSessions(shipUrl: string, segment: string, name: string): Promise<AgentSession[]> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/sessions`);
			if (!response.ok) throw new Error('Failed to load sessions');
			const data: WorkspaceSessionsResponse = await response.json();
			return [...(data.sessions ?? [])].reverse();
		},
		/**
		 * Run `breq complete` / `breq abort` for a workspace; resolves to the window it runs in.
		 *
		 * The daemon holds the pane rather than answering with the script's output, so the caller's
		 * job after this is to go and look at that window.
		 */
		async runWorkflow(
			shipUrl: string,
			segment: string,
			name: string,
			verb: WorkflowVerb,
		): Promise<string> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/workflow`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ verb } satisfies WorkflowRequest),
			});
			if (!response.ok) {
				const data = await response.json().catch(() => ({}));
				throw new Error(data.error || `Failed to run ${verb}`);
			}
			const data: WorkflowResponse = await response.json();
			return data.window;
		},
		/** Dismiss one window of the workspace's session — a held pane, usually. */
		async closeWorkspaceWindow(
			shipUrl: string,
			segment: string,
			name: string,
			window: string,
		): Promise<void> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const win = encodeURIComponent(window);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/windows/${win}/close`, {
				method: 'POST',
			});
			if (!response.ok) {
				const data = await response.json().catch(() => ({}));
				throw new Error(data.error || 'Failed to close window');
			}
		},
		async stopWorkspace(shipUrl: string, segment: string, name: string): Promise<void> {
			const seg = encodeURIComponent(segment);
			const ws = encodeURIComponent(name);
			const response = await fetch(`${shipUrl}/api/workspaces/${seg}/${ws}/stop`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
			});
			if (!response.ok) {
				const data = await response.json().catch(() => ({}));
				throw new Error(data.error || 'Failed to stop workspace');
			}
		},
	};
}

export const torenStore = createTorenStore();

// Derived stores
export const isConnected = derived(torenStore, ($toren) => $toren.connected);
export const isAuthenticated = derived(torenStore, ($toren) => $toren.authenticated);
export const messages = derived(torenStore, ($toren) => $toren.messages);
export const workspaces = derived(torenStore, ($toren) => $toren.workspaces);

// Filter workspaces for the current segment and sort by name
export const segmentWorkspaces = derived(torenStore, ($toren) => {
	if (!$toren.selectedSegment) return [];
	const segmentName = $toren.selectedSegment.name.toLowerCase();
	return $toren.workspaces
		.filter((w) => w.segment.toLowerCase() === segmentName)
		.sort((a, b) => a.name.localeCompare(b.name));
});
