<script lang="ts">
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { createEventDispatcher, onDestroy, onMount } from 'svelte';
import { fitTerminal } from '$lib/terminal/fit';
import { decodeFrame, EpochFilter } from '$lib/terminal/frames';
import { type HeldAction, heldAction } from '$lib/terminal/held';

/** Full websocket URL for the workspace's pane bridge, or null to stay disconnected. */
export let url: string | null;
/**
 * Bumped by the parent when the pane behind an unchanged `url` has been replaced — resuming an
 * agent puts a new pane in the same window, and the url has no way to say so.
 */
// biome-ignore lint/style/useConst: svelte props are reassigned by the parent
export let attachNonce = 0;
/**
 * Whether the pane has exited and is being held. Its keys stop being typed at and start meaning
 * the three things its status line offers.
 */
// biome-ignore lint/style/useConst: svelte props are reassigned by the parent
export let held = false;

const dispatch = createEventDispatcher<{
	status: { status: string; session?: string };
	error: { message: string };
	held: { action: HeldAction };
}>();

let host: HTMLDivElement;
let term: Terminal | null = null;
let socket: WebSocket | null = null;
let resizeObserver: ResizeObserver | null = null;
let fitFrame: number | null = null;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let reconnectAttempts = 0;
let pingTimer: ReturnType<typeof setInterval> | null = null;
let awaitingPong = false;
/** Whether the daemon has told us this pane is over, which is why the socket closed. */
let paneEnded = false;
const epochs = new EpochFilter();

// The bridge closes when its pane goes away; reconnecting picks up whatever replaced it, rather
// than sitting frozen on the old agent's last frame. A pane the daemon calls over is the one case
// that is not retried — `attachNonce` is what re-attaches that one.
const RECONNECT_DELAYS_MS = [500, 1000, 2000, 4000, 8000];
const MAX_RECONNECT_ATTEMPTS = 20;

// A pane can go hours without a byte, so nothing else distinguishes a quiet agent from a socket a
// proxy or a sleeping phone dropped. The daemon pings us; this is the other direction, as JSON,
// because the browser API will not send a protocol ping.
const PING_INTERVAL_MS = 20_000;

/**
 * Connect once the terminal exists, and reconnect whenever the target — or the pane behind an
 * unchanged one — changes.
 */
$: if (term) {
	connect(url, attachNonce);
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

	term.open(host);
	refit();
	// The grid is measured in cells, and a cell is measured from the font: until it has loaded,
	// every row count derived from it is provisional and nothing about the layout says so.
	document.fonts?.ready.then(scheduleFit);

	term.onData((data) => {
		if (held) {
			// Nothing is listening in the pane; the keys act on it from out here instead.
			const action = heldAction(data);
			if (action) dispatch('held', { action });
			return;
		}
		send({ type: 'data', data });
	});

	// The host is the box the rows are laid out in, so it is the box to measure: every bar above
	// the terminal changes its height, and none of them is worth knowing about individually.
	resizeObserver = new ResizeObserver(scheduleFit);
	resizeObserver.observe(host);
});

onDestroy(() => {
	resizeObserver?.disconnect();
	if (fitFrame !== null) cancelAnimationFrame(fitFrame);
	fitFrame = null;
	disconnect();
	term?.dispose();
	term = null;
});

/** Coalesce the fits a single layout change can ask for into one, after that layout has settled. */
function scheduleFit() {
	if (fitFrame !== null) return;
	fitFrame = requestAnimationFrame(() => {
		fitFrame = null;
		refit();
	});
}

/** Size the grid to the host, and tell the pane what it now is. */
function refit() {
	if (!term || !host) return;
	const geometry = fitTerminal(term, host);
	if (!geometry) return;
	send({ type: 'resize', cols: geometry.cols, rows: geometry.rows });
}

function scheduleReconnect(target: string) {
	if (reconnectTimer) return;
	if (reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
		// Not `ended`: giving up says nothing about the pane, which is very likely still running
		// behind a daemon that went away. Only the daemon gets to call a pane over.
		dispatch('status', { status: 'unreachable' });
		return;
	}
	const delay = RECONNECT_DELAYS_MS[Math.min(reconnectAttempts, RECONNECT_DELAYS_MS.length - 1)];
	reconnectAttempts += 1;
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		if (url === target) open(target);
	}, delay);
}

function connect(target: string | null, _nonce: number) {
	disconnect();
	reconnectAttempts = 0;
	paneEnded = false;
	if (!target || !term) return;

	term.reset();
	epochs.reset();
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
		startKeepalive(ws);
	};

	ws.onmessage = (event) => {
		// Anything at all proves the connection is alive, whatever it says.
		awaitingPong = false;
		if (typeof event.data === 'string') {
			handleControlMessage(event.data);
			return;
		}
		handleFrame(event.data as ArrayBuffer);
	};

	// A fresh pane redraws from scratch; don't interleave it with the old one's output.
	if (reconnectAttempts > 0) {
		term.reset();
		epochs.reset();
	}

	ws.onerror = () => {
		dispatch('error', { message: 'Terminal connection error' });
	};

	ws.onclose = () => {
		if (socket !== ws) return;
		stopKeepalive();
		// A held pane is over, not lost: reconnecting to it would only be told so again, and the
		// last frame it left on screen is exactly what the surface should keep showing until it
		// starts something in its place — which is when the parent bumps `attachNonce`.
		if (paneEnded) {
			dispatch('status', { status: 'ended' });
			return;
		}
		dispatch('status', { status: 'disconnected' });
		scheduleReconnect(target);
	};
}

/**
 * Apply a frame, unless it describes a screen we have already been moved off.
 *
 * A new epoch is a fresh paint of the whole pane, so the terminal is cleared first: the paint
 * fixes the grid, and the reset is what clears out whatever mode the old screen left us in.
 */
function handleFrame(data: ArrayBuffer) {
	const frame = decodeFrame(data);
	if (!frame) return;
	const action = epochs.accept(frame);
	if (action === 'discard') return;
	if (action === 'repaint') term?.reset();
	term?.write(frame.bytes);
}

function handleControlMessage(raw: string) {
	try {
		const msg = JSON.parse(raw);
		if (msg.type === 'status') {
			paneEnded = msg.status === 'ended';
			dispatch('status', { status: msg.status, session: msg.session });
		} else if (msg.type === 'error') {
			dispatch('error', { message: msg.message });
		}
	} catch {
		console.warn('Unparseable terminal control message:', raw);
	}
}

/** Ping on an interval, and treat an unanswered ping as a dead socket rather than a quiet pane. */
function startKeepalive(ws: WebSocket) {
	stopKeepalive();
	awaitingPong = false;
	pingTimer = setInterval(() => {
		if (socket !== ws || ws.readyState !== WebSocket.OPEN) return;
		if (awaitingPong) {
			// Closing is what starts the reconnect; the socket is gone whether or not it says so.
			ws.close();
			return;
		}
		awaitingPong = true;
		send({ type: 'ping' });
	}, PING_INTERVAL_MS);
}

function stopKeepalive() {
	if (pingTimer) {
		clearInterval(pingTimer);
		pingTimer = null;
	}
	awaitingPong = false;
}

function disconnect() {
	if (reconnectTimer) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	stopKeepalive();
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

/** Ask the daemon to repaint this pane, for a terminal that looks wrong. */
export function resync() {
	send({ type: 'resync' });
}
</script>

<div class="terminal-frame">
	<div class="terminal-host" bind:this={host}></div>
</div>

<style>
/*
 * The frame holds the breathing room, and the host holds nothing but the grid: the fit measures
 * the host, so any padding on it would be counted as room for rows that are then drawn outside it.
 */
.terminal-frame {
	flex: 1;
	min-height: 0;
	display: flex;
	flex-direction: column;
	padding: 0.5rem;
	background: #0d0f12;
}

.terminal-host {
	flex: 1;
	min-height: 0;
	min-width: 0;
	overflow: hidden;
}
</style>
