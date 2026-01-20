import { derived, writable } from 'svelte/store';
import type {
	Ancillary,
	Assignment,
	CommandOutput,
	Segment,
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
	ancillaries: Ancillary[];
	assignments: Assignment[];
	messages: ChatMessage[];
	segments: Segment[];
	segmentRoots: string[];
	selectedSegment: Segment | null;
	selectedAncillary: Assignment | null;
	loadingSegments: boolean;
	loadingAssignments: boolean;
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
	private reconnectAttempts = 0;
	private maxReconnectAttempts = 5;
	private reconnectDelay = 1000;

	constructor() {}

	async connect(shipUrl: string): Promise<void> {
		return new Promise((resolve, reject) => {
			const wsUrl = shipUrl.replace(/^http/, 'ws') + '/ws';

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
					this.reconnectAttempts = 0;
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

					// Attempt reconnect
					if (this.reconnectAttempts < this.maxReconnectAttempts) {
						this.reconnectAttempts++;
						const delay = this.reconnectDelay * this.reconnectAttempts;
						console.log(`Reconnecting in ${delay}ms...`);
						setTimeout(() => {
							const state = torenStore.get();
							this.connect(state.shipUrl);
						}, delay);
					}
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
			const unsubscribe = torenStore.subscribe((state) => {
				// This is a hack - we should use proper event emitter
				// For now, messages are handled in handleMessage
			});

			this.send({ type: 'Auth', token });

			// Store handler for later
			(this as any)._authHandler = handler;
		});
	}

	private handleMessage(message: WsResponse): void {
		console.log('Received message:', message);

		// Handle auth responses
		if ((this as any)._authHandler) {
			(this as any)._authHandler(message);
			delete (this as any)._authHandler;
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

// Create the store with a custom store that includes helper methods
function createTorenStore() {
	const initialState: TorenState = {
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

	const { subscribe, set, update } = writable(initialState);

	return {
		subscribe,
		set,
		update,
		get: () => {
			let state: TorenState;
			subscribe((s) => (state = s))();
			return state!;
		},
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
		async loadAssignments(shipUrl: string) {
			update((state) => ({ ...state, loadingAssignments: true }));
			try {
				const response = await fetch(`${shipUrl}/api/assignments`);
				if (!response.ok) throw new Error('Failed to fetch assignments');
				const data = await response.json();
				update((state) => ({
					...state,
					assignments: data.assignments,
					loadingAssignments: false,
				}));
			} catch (error) {
				console.error('Failed to load assignments:', error);
				update((state) => ({
					...state,
					loadingAssignments: false,
				}));
			}
		},
		selectAncillary(assignment: Assignment | null) {
			update((state) => ({ ...state, selectedAncillary: assignment }));
		},
	};
}

export const torenStore = createTorenStore();

// Derived stores
export const isConnected = derived(torenStore, ($toren) => $toren.connected);
export const isAuthenticated = derived(torenStore, ($toren) => $toren.authenticated);
export const messages = derived(torenStore, ($toren) => $toren.messages);
export const assignments = derived(torenStore, ($toren) => $toren.assignments);

// Filter assignments for current segment
export const segmentAssignments = derived(torenStore, ($toren) => {
	if (!$toren.selectedSegment) return [];
	const segmentName = $toren.selectedSegment.name.toLowerCase();
	return $toren.assignments.filter((a) => a.segment.toLowerCase() === segmentName);
});
