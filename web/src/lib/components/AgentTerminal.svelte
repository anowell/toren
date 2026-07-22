<script lang="ts">
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { createEventDispatcher, onDestroy, onMount } from 'svelte';

/** Full websocket URL for the ancillary's pane bridge, or null to stay disconnected. */
export let url: string | null;

const dispatch = createEventDispatcher<{
	status: { status: string; session?: string };
	error: { message: string };
}>();

let host: HTMLDivElement;
let term: Terminal | null = null;
let fit: FitAddon | null = null;
let socket: WebSocket | null = null;
let resizeObserver: ResizeObserver | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempts = 0;

// The bridge closes when its pane goes away; reconnecting picks up whatever replaced it, rather
// than sitting frozen on the old agent's last frame.
const RECONNECT_DELAYS_MS = [500, 1000, 2000, 4000, 8000];
const MAX_RECONNECT_ATTEMPTS = 20;

/** Connect once the terminal exists, and reconnect whenever the target changes. */
$: if (term) {
	connect(url);
}

onMount(() => {
	term = new Terminal({
		convertEol: false,
		cursorBlink: true,
		fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
		fontSize: 13,
		scrollback: 20000,
		theme: {
			background: '#0d0f12',
			foreground: '#d8dee9',
			cursor: '#88c0d0',
		},
	});

	fit = new FitAddon();
	term.loadAddon(fit);
	term.open(host);
	fit.fit();

	term.onData((data) => {
		send({ type: 'data', data });
	});

	resizeObserver = new ResizeObserver(() => {
		if (!fit || !term) return;
		fit.fit();
		send({ type: 'resize', cols: term.cols, rows: term.rows });
	});
	resizeObserver.observe(host);
});

onDestroy(() => {
	resizeObserver?.disconnect();
	disconnect();
	term?.dispose();
	term = null;
});

function scheduleReconnect(target: string) {
	if (reconnectTimer) return;
	if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
		dispatch('status', { status: 'ended' });
		return;
	}
	const delay = RECONNECT_DELAYS_MS[Math.min(reconnectAttempts, RECONNECT_DELAYS_MS.length - 1)];
	reconnectAttempts += 1;
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		if (url === target) open(target);
	}, delay);
}

function connect(target: string | null) {
	disconnect();
	reconnectAttempts = 0;
	if (!target || !term) return;

	term.reset();
	open(target);
}

function open(target: string) {
	if (!term) return;

	const ws = new WebSocket(target);
	ws.binaryType = 'arraybuffer';
	socket = ws;

	ws.onopen = () => {
		reconnectAttempts = 0;
		if (term) send({ type: 'resize', cols: term.cols, rows: term.rows });
	};

	ws.onmessage = (event) => {
		if (typeof event.data === 'string') {
			handleControlMessage(event.data);
			return;
		}
		term?.write(new Uint8Array(event.data as ArrayBuffer));
	};

	// A fresh pane redraws from scratch; don't interleave it with the old one's output.
	if (reconnectAttempts > 0) term.reset();

	ws.onerror = () => {
		dispatch('error', { message: 'Terminal connection error' });
	};

	ws.onclose = () => {
		if (socket !== ws) return;
		dispatch('status', { status: 'disconnected' });
		scheduleReconnect(target);
	};
}

function handleControlMessage(raw: string) {
	try {
		const msg = JSON.parse(raw);
		if (msg.type === 'status') {
			dispatch('status', { status: msg.status, session: msg.session });
		} else if (msg.type === 'error') {
			dispatch('error', { message: msg.message });
		}
	} catch {
		console.warn('Unparseable terminal control message:', raw);
	}
}

function disconnect() {
	if (reconnectTimer) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	if (socket) {
		socket.onclose = null;
		socket.close();
		socket = null;
	}
}

function send(message: Record<string, unknown>) {
	if (socket?.readyState === WebSocket.OPEN) {
		socket.send(JSON.stringify(message));
	}
}

/** Type a line into the pane and submit it. Used by the on-screen input, chiefly for mobile. */
export function sendLine(text: string) {
	send({ type: 'data', data: `${text}\r` });
}

export function interrupt() {
	send({ type: 'interrupt' });
}
</script>

<div class="terminal-host" bind:this={host}></div>

<style>
.terminal-host {
	width: 100%;
	height: 100%;
	min-height: 0;
	padding: 0.5rem;
	background: #0d0f12;
	overflow: hidden;
}

.terminal-host :global(.xterm) {
	height: 100%;
}
</style>
