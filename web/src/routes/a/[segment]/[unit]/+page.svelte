<script lang="ts">
import { afterUpdate, onDestroy, tick } from 'svelte';
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import BeadStatusIcon from '$lib/components/BeadStatusIcon.svelte';
import SegmentDropdown from '$lib/components/SegmentDropdown.svelte';
import { connectionStore } from '$lib/stores/connection';
import {
	getAncillaryDisplayStatus,
	getBeadDisplayStatus,
	segmentAssignments,
	stripBeadPrefix,
	torenStore,
} from '$lib/stores/toren';
import type { AncillaryWsResponse, WorkEvent, WorkOp } from '$lib/types/toren';

let messageInput = '';
let showMobilePanel = false;

// Work event state
let events: WorkEvent[] = [];
let workStatus: string = 'connecting';
let wsError: string | null = null;
let ancillaryWs: WebSocket | null = null;
let messagesContainer: HTMLDivElement;
let autoScroll = true;

function goToSegmentSelector() {
	torenStore.selectSegment(null);
	goto('/');
}

// Get current assignment based on unit param
$: currentAssignment = $segmentAssignments.find((a) => {
	const unitName = a.ancillary_id.split(' ').pop()?.toLowerCase();
	return unitName === $page.params.unit?.toLowerCase();
});

// Build the ancillary ID for WebSocket connection
$: ancillaryId = currentAssignment?.ancillary_id ?? null;

// Look up polled ancillary display status
$: ancillaryDisplayStatus = (() => {
	if (!ancillaryId) return 'ready' as const;
	const ancillary = $torenStore.ancillaries.find((a) => a.id === ancillaryId);
	if (!ancillary) return 'ready' as const;
	return getAncillaryDisplayStatus(ancillary.status);
})();

// Bead display status
$: beadDisplayStatus = currentAssignment ? getBeadDisplayStatus(currentAssignment.status) : null;

// Connect/reconnect when ancillary changes or auth state changes
// Gate on authenticated — only open ancillary WS when main connection is up
$: if (ancillaryId && $torenStore.authenticated) {
	connectToAncillary(ancillaryId);
} else {
	disconnectAncillary();
}

function connectToAncillary(id: string) {
	disconnectAncillary();
	events = [];
	workStatus = 'connecting';
	wsError = null;

	const shipUrl = $torenStore.shipUrl;
	const wsUrl = shipUrl.replace(/^http/, 'ws');
	const encoded = encodeURIComponent(id);

	const ws = new WebSocket(`${wsUrl}/ws/ancillaries/${encoded}`);
	ancillaryWs = ws;

	ws.onopen = () => {
		workStatus = 'connected';
	};

	ws.onmessage = (event) => {
		try {
			const msg: AncillaryWsResponse = JSON.parse(event.data);
			handleAncillaryMessage(msg);
		} catch (err) {
			console.error('Failed to parse ancillary WS message:', err);
		}
	};

	ws.onerror = () => {
		wsError = 'Connection error';
		workStatus = 'disconnected';
	};

	ws.onclose = () => {
		if (ancillaryWs === ws) {
			workStatus = 'disconnected';
		}
	};
}

function disconnectAncillary() {
	if (ancillaryWs) {
		ancillaryWs.close();
		ancillaryWs = null;
	}
}

function handleAncillaryMessage(msg: AncillaryWsResponse) {
	switch (msg.type) {
		case 'event':
			events = [...events, msg.event];
			break;
		case 'replay_complete':
			workStatus = 'live';
			break;
		case 'status':
			workStatus = msg.status;
			break;
		case 'error':
			wsError = msg.message;
			break;
	}
}

onDestroy(() => {
	disconnectAncillary();
});

// Auto-scroll to bottom when new events arrive
afterUpdate(() => {
	if (autoScroll && messagesContainer) {
		messagesContainer.scrollTop = messagesContainer.scrollHeight;
	}
});

function handleScroll() {
	if (!messagesContainer) return;
	const { scrollTop, scrollHeight, clientHeight } = messagesContainer;
	autoScroll = scrollHeight - scrollTop - clientHeight < 50;
}

function handleSendMessage() {
	if (!messageInput.trim() || !ancillaryWs || ancillaryWs.readyState !== WebSocket.OPEN) return;

	const content = messageInput.trim();
	messageInput = '';

	ancillaryWs.send(JSON.stringify({ type: 'message', content }));
}

function handleInterrupt() {
	if (!ancillaryWs || ancillaryWs.readyState !== WebSocket.OPEN) return;
	ancillaryWs.send(JSON.stringify({ type: 'interrupt' }));
}

function toggleMobilePanel() {
	showMobilePanel = !showMobilePanel;
}

function closeMobilePanel() {
	showMobilePanel = false;
}

function navigateToAncillary(ancillaryId: string) {
	const parts = ancillaryId.split(' ');
	const unit = parts[parts.length - 1].toLowerCase();
	goto(`/a/${$page.params.segment}/${unit}`);
	closeMobilePanel();
}

function navigateToNewAncillary() {
	goto(`/a/${$page.params.segment}`);
	closeMobilePanel();
}

function capitalizeUnit(unit: string): string {
	return unit.charAt(0).toUpperCase() + unit.slice(1);
}

function lookupAncillaryDisplayStatus(ancillaryId: string): 'busy' | 'ready' {
	const ancillary = $torenStore.ancillaries.find((a) => a.id === ancillaryId);
	if (!ancillary) return 'ready';
	return getAncillaryDisplayStatus(ancillary.status);
}

// Group consecutive events into display items
interface DisplayItem {
	type: 'assistant' | 'user' | 'tool' | 'status' | 'error';
	content: string;
	detail?: string;
	seq: number;
}

$: displayItems = buildDisplayItems(events);

function buildDisplayItems(events: WorkEvent[]): DisplayItem[] {
	const items: DisplayItem[] = [];

	for (const event of events) {
		const op = event.op;
		switch (op.type) {
			case 'assistant_message':
				// Merge consecutive assistant messages
				if (items.length > 0 && items[items.length - 1].type === 'assistant') {
					items[items.length - 1].content += `\n${op.content}`;
				} else {
					items.push({ type: 'assistant', content: op.content, seq: event.seq });
				}
				break;
			case 'user_message':
				items.push({ type: 'user', content: op.content, seq: event.seq });
				break;
			case 'tool_call':
				items.push({
					type: 'tool',
					content: op.name,
					detail:
						typeof op.input === 'object' ? summarizeToolInput(op.name, op.input) : String(op.input),
					seq: event.seq,
				});
				break;
			case 'assignment_started':
				items.push({
					type: 'status',
					content: `Started working on ${op.bead_id}`,
					seq: event.seq,
				});
				break;
			case 'assignment_completed':
				items.push({ type: 'status', content: 'Work completed', seq: event.seq });
				break;
			case 'assignment_failed':
				items.push({ type: 'error', content: `Failed: ${op.error}`, seq: event.seq });
				break;
			case 'status_change':
				items.push({ type: 'status', content: `Status: ${op.status}`, seq: event.seq });
				break;
			// Skip other event types (thinking, file ops, command output, client events)
		}
	}
	return items;
}

function summarizeToolInput(_name: string, input: unknown): string {
	if (!input || typeof input !== 'object') return '';
	const obj = input as Record<string, unknown>;

	// Show the most relevant field for common tools
	if (obj.file_path) return String(obj.file_path);
	if (obj.path) return String(obj.path);
	if (obj.command) return String(obj.command);
	if (obj.pattern) return String(obj.pattern);
	if (obj.query) return String(obj.query);

	// Fallback: show first key=value
	const keys = Object.keys(obj);
	if (keys.length === 0) return '';
	return `${keys[0]}: ${String(obj[keys[0]]).slice(0, 60)}`;
}

$: isWorking = workStatus === 'working' || workStatus === 'live' || workStatus === 'connected';
$: isDone = workStatus === 'completed' || workStatus.startsWith('failed');
</script>

<div class="chat-view">
	<!-- Header -->
	<header class="chat-header">
		<div class="header-left">
			<button class="logo-link" on:click={goToSegmentSelector}>
				<span class="logo">Toren</span>
			</button>
			{#if $torenStore.selectedSegment}
				<SegmentDropdown />
			{/if}
		</div>
		<div class="header-right">
			{#if isWorking}
				<button class="interrupt-btn" on:click={handleInterrupt} title="Interrupt">
					<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
						<rect x="6" y="6" width="12" height="12" rx="2" />
					</svg>
				</button>
			{/if}
			<div class="status">
				<span
					class="status-dot"
					class:connected={$connectionStore.phase === 'connected'}
					class:reconnecting={$connectionStore.phase === 'connecting' || $connectionStore.phase === 'authenticating'}
				></span>
				<span class="status-text">
					{#if $connectionStore.phase === 'connected'}Connected
					{:else if $connectionStore.phase === 'connecting' || $connectionStore.phase === 'authenticating'}Reconnecting...
					{:else}Disconnected{/if}
				</span>
			</div>
		</div>
	</header>

	<!-- Ancillary indicator -->
	{#if currentAssignment}
		<div class="ancillary-indicator">
			<span class="ancillary-name">{currentAssignment.ancillary_id}</span>
			<span class="separator">·</span>
			{#if beadDisplayStatus}
				<BeadStatusIcon status={beadDisplayStatus} />
			{/if}
			<span class="bead-label">{stripBeadPrefix(currentAssignment.bead_id)}{#if currentAssignment.bead_title}: {currentAssignment.bead_title}{/if}</span>
			<span class="ancillary-display-badge" class:busy={ancillaryDisplayStatus === 'busy'} class:ready={ancillaryDisplayStatus === 'ready'}>
				{ancillaryDisplayStatus}
			</span>
		</div>
	{:else}
		<div class="ancillary-indicator not-found">
			<span>{$torenStore.selectedSegment?.name} {capitalizeUnit($page.params.unit ?? '')}</span>
			<span class="hint">No active assignment</span>
		</div>
	{/if}

	<!-- Messages area -->
	<div class="chat-messages" bind:this={messagesContainer} on:scroll={handleScroll}>
		{#if !currentAssignment}
			<div class="empty-state">
				<div class="empty-icon">?</div>
				<h2>No Active Assignment</h2>
				<p>This ancillary doesn't have an active task.</p>
			</div>
		{:else if displayItems.length === 0 && workStatus === 'connecting'}
			<div class="empty-state">
				<div class="empty-icon spinning">...</div>
				<h2>Connecting</h2>
				<p>Connecting to {currentAssignment.ancillary_id}...</p>
			</div>
		{:else if displayItems.length === 0 && wsError}
			<div class="empty-state">
				<div class="empty-icon">!</div>
				<h2>Not Available</h2>
				<p>{wsError}</p>
			</div>
		{:else}
			{#each displayItems as item (item.seq + '-' + item.type)}
				{#if item.type === 'assistant'}
					<div class="message message-assistant">
						<div class="message-content">{item.content}</div>
					</div>
				{:else if item.type === 'user'}
					<div class="message message-user">
						<div class="message-content">{item.content}</div>
					</div>
				{:else if item.type === 'tool'}
					<div class="message message-tool">
						<span class="tool-name">{item.content}</span>
						{#if item.detail}
							<span class="tool-detail">{item.detail}</span>
						{/if}
					</div>
				{:else if item.type === 'status'}
					<div class="message message-status">{item.content}</div>
				{:else if item.type === 'error'}
					<div class="message message-error">{item.content}</div>
				{/if}
			{/each}
			{#if isWorking}
				<div class="message message-status thinking">Working...</div>
			{/if}
		{/if}
	</div>

	<!-- Input area -->
	<div class="chat-input">
		<button class="panel-toggle mobile-only" on:click={toggleMobilePanel} aria-label="View ancillaries">
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="20"
				height="20"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
				<circle cx="9" cy="7" r="4"></circle>
				<path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
				<path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
			</svg>
			{#if $segmentAssignments.length > 0}
				<span class="badge">{$segmentAssignments.length}</span>
			{/if}
		</button>
		<form on:submit|preventDefault={handleSendMessage}>
			<textarea
				bind:value={messageInput}
				placeholder="Send an instruction..."
				rows="1"
				disabled={!ancillaryWs || ancillaryWs.readyState !== WebSocket.OPEN}
				on:keydown={(e) => {
					if (e.key === 'Enter' && !e.shiftKey) {
						e.preventDefault();
						handleSendMessage();
					}
				}}
			></textarea>
			<button type="submit" disabled={!messageInput.trim() || !ancillaryWs || ancillaryWs.readyState !== WebSocket.OPEN} aria-label="Send message">
				<svg
					xmlns="http://www.w3.org/2000/svg"
					width="20"
					height="20"
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<line x1="22" y1="2" x2="11" y2="13"></line>
					<polygon points="22 2 15 22 11 13 2 9 22 2"></polygon>
				</svg>
			</button>
		</form>
	</div>
</div>

<!-- Mobile panel overlay -->
{#if showMobilePanel}
	<div class="mobile-overlay" on:click={closeMobilePanel} role="presentation">
		<div class="mobile-panel" on:click|stopPropagation role="dialog">
			<div class="mobile-panel-header">
				<h3>Ancillaries</h3>
				<button class="close-btn" on:click={closeMobilePanel} aria-label="Close">
					<svg
						xmlns="http://www.w3.org/2000/svg"
						width="20"
						height="20"
						viewBox="0 0 24 24"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						stroke-linecap="round"
						stroke-linejoin="round"
					>
						<line x1="18" y1="6" x2="6" y2="18"></line>
						<line x1="6" y1="6" x2="18" y2="18"></line>
					</svg>
				</button>
			</div>
			<div class="mobile-panel-list">
				<!-- New Ancillary option -->
				<button class="mobile-item" on:click={navigateToNewAncillary}>
					<div class="item-main">
						<svg
							xmlns="http://www.w3.org/2000/svg"
							width="16"
							height="16"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="2"
							stroke-linecap="round"
							stroke-linejoin="round"
						>
							<line x1="12" y1="5" x2="12" y2="19"></line>
							<line x1="5" y1="12" x2="19" y2="12"></line>
						</svg>
						<span>New Ancillary</span>
					</div>
				</button>

				{#each $segmentAssignments as assignment (assignment.id)}
					{@const unitName = assignment.ancillary_id.split(' ').pop()?.toLowerCase()}
					{@const displayStatus = lookupAncillaryDisplayStatus(assignment.ancillary_id)}
					{@const beadStatus = getBeadDisplayStatus(assignment.status)}
					<button
						class="mobile-item"
						class:selected={unitName === $page.params.unit?.toLowerCase()}
						on:click={() => navigateToAncillary(assignment.ancillary_id)}
					>
						<div class="item-main">
							<span class="ancillary-status-dot" class:busy={displayStatus === 'busy'} class:ready={displayStatus === 'ready'}></span>
							<span class="item-name">{assignment.ancillary_id}</span>
						</div>
						<span class="item-bead"><BeadStatusIcon status={beadStatus} /> {stripBeadPrefix(assignment.bead_id)}{#if assignment.bead_title}: {assignment.bead_title}{/if}</span>
					</button>
				{/each}
			</div>
		</div>
	</div>
{/if}

<style>
	.chat-view {
		display: flex;
		flex-direction: column;
		height: 100%;
		width: 100%;
		background: var(--color-bg);
	}

	/* Header */
	.chat-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		flex-shrink: 0;
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: var(--spacing-md);
	}

	.logo-link {
		text-decoration: none;
		background: none;
		border: none;
		cursor: pointer;
		padding: 0;
	}

	.logo {
		font-size: 1.25rem;
		font-weight: 700;
		color: var(--color-primary);
	}

	.header-right {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
	}

	.interrupt-btn {
		width: 32px;
		height: 32px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-error);
		border: none;
		border-radius: var(--radius-sm);
		color: white;
		cursor: pointer;
	}

	.interrupt-btn:hover {
		opacity: 0.8;
	}

	.status {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
	}

	.status-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--color-error);
	}

	.status-dot.connected {
		background: var(--color-success);
	}

	.status-dot.reconnecting {
		background: var(--color-warning);
	}

	.status-text {
		font-size: 0.8rem;
		color: var(--color-text-secondary);
	}

	/* Ancillary indicator */
	.ancillary-indicator {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border-bottom: 1px solid var(--color-border);
		font-size: 0.85rem;
		flex-shrink: 0;
	}

	.ancillary-indicator.not-found {
		color: var(--color-text-secondary);
	}

	.ancillary-name {
		font-weight: 600;
		color: var(--color-text);
	}

	.separator {
		color: var(--color-text-secondary);
	}

	.bead-label {
		color: var(--color-text-secondary);
		font-size: 0.8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.ancillary-display-badge {
		margin-left: auto;
		padding: 2px 8px;
		border-radius: var(--radius-sm);
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
	}

	.ancillary-display-badge.busy {
		background: var(--color-warning);
		color: var(--color-bg);
	}

	.ancillary-display-badge.ready {
		background: var(--color-success);
		color: var(--color-bg);
	}

	.ancillary-indicator .hint {
		color: var(--color-text-secondary);
		font-size: 0.75rem;
		margin-left: auto;
	}

	/* Messages */
	.chat-messages {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-md);
		display: flex;
		flex-direction: column;
		gap: var(--spacing-sm);
	}

	.empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		height: 100%;
		text-align: center;
		color: var(--color-text-secondary);
	}

	.empty-icon {
		width: 64px;
		height: 64px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 2rem;
		border: 2px solid var(--color-border);
		border-radius: 50%;
		margin-bottom: var(--spacing-md);
	}

	.empty-icon.spinning {
		animation: spin 1.5s linear infinite;
		border-style: solid;
		border-color: var(--color-primary) transparent transparent transparent;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	.empty-state h2 {
		margin: 0 0 var(--spacing-sm) 0;
		color: var(--color-text);
		font-size: 1.25rem;
	}

	.empty-state p {
		margin: 0;
		max-width: 300px;
	}

	/* Message styles */
	.message {
		max-width: 100%;
	}

	.message-assistant {
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		padding: var(--spacing-sm) var(--spacing-md);
	}

	.message-content {
		white-space: pre-wrap;
		word-break: break-word;
		font-size: 0.9rem;
		line-height: 1.5;
	}

	.message-user {
		background: var(--color-primary);
		color: white;
		border-radius: var(--radius-md);
		padding: var(--spacing-sm) var(--spacing-md);
		align-self: flex-end;
		max-width: 80%;
	}

	.message-tool {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-sm);
		font-size: 0.8rem;
		color: var(--color-text-secondary);
		border-left: 2px solid var(--color-border);
	}

	.tool-name {
		font-family: var(--font-mono);
		font-weight: 600;
		color: var(--color-text);
	}

	.tool-detail {
		font-family: var(--font-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 300px;
	}

	.message-status {
		text-align: center;
		font-size: 0.8rem;
		color: var(--color-text-secondary);
		padding: var(--spacing-xs) 0;
	}

	.message-status.thinking {
		animation: pulse 1.5s ease-in-out infinite;
	}

	@keyframes pulse {
		0%, 100% { opacity: 0.5; }
		50% { opacity: 1; }
	}

	.message-error {
		text-align: center;
		font-size: 0.8rem;
		color: var(--color-error);
		padding: var(--spacing-xs) 0;
	}

	/* Input */
	.chat-input {
		display: flex;
		align-items: flex-end;
		gap: var(--spacing-sm);
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-secondary);
		border-top: 1px solid var(--color-border);
		flex-shrink: 0;
	}

	.panel-toggle {
		width: 44px;
		height: 44px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text-secondary);
		position: relative;
		flex-shrink: 0;
	}

	.panel-toggle:hover {
		border-color: var(--color-primary);
		color: var(--color-text);
	}

	.panel-toggle .badge {
		position: absolute;
		top: -4px;
		right: -4px;
		min-width: 18px;
		height: 18px;
		padding: 0 4px;
		background: var(--color-warning);
		color: var(--color-bg);
		font-size: 0.7rem;
		font-weight: 700;
		border-radius: 9px;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	form {
		flex: 1;
		display: flex;
		gap: var(--spacing-sm);
		align-items: flex-end;
	}

	textarea {
		flex: 1;
		min-height: 44px;
		max-height: 150px;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		resize: none;
		font-size: 1rem;
		line-height: 1.4;
		color: var(--color-text);
	}

	textarea:focus {
		border-color: var(--color-primary);
		outline: none;
	}

	textarea:disabled {
		opacity: 0.5;
	}

	button[type='submit'] {
		width: 44px;
		height: 44px;
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--color-primary);
		border-radius: var(--radius-md);
		color: white;
		flex-shrink: 0;
	}

	button[type='submit']:hover:not(:disabled) {
		background: var(--color-primary-hover);
	}

	button[type='submit']:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	/* Mobile panel */
	.mobile-only {
		display: flex;
	}

	.mobile-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		z-index: 100;
		display: flex;
		align-items: flex-end;
	}

	.mobile-panel {
		width: 100%;
		max-height: 70vh;
		background: var(--color-bg);
		border-radius: var(--radius-lg) var(--radius-lg) 0 0;
		display: flex;
		flex-direction: column;
		animation: slideUp 0.2s ease-out;
	}

	@keyframes slideUp {
		from {
			transform: translateY(100%);
		}
		to {
			transform: translateY(0);
		}
	}

	.mobile-panel-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--spacing-md);
		border-bottom: 1px solid var(--color-border);
	}

	.mobile-panel-header h3 {
		margin: 0;
		font-size: 1rem;
		color: var(--color-text);
	}

	.close-btn {
		width: 36px;
		height: 36px;
		display: flex;
		align-items: center;
		justify-content: center;
		border-radius: var(--radius-sm);
		color: var(--color-text-secondary);
	}

	.close-btn:hover {
		background: var(--color-bg-tertiary);
		color: var(--color-text);
	}

	.mobile-panel-list {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-sm);
	}

	.mobile-item {
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
		width: 100%;
		padding: var(--spacing-md);
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		text-align: left;
		margin-bottom: var(--spacing-sm);
	}

	.mobile-item:hover,
	.mobile-item.selected {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.item-main {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
	}

	.ancillary-status-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex-shrink: 0;
	}

	.ancillary-status-dot.ready {
		background: var(--color-success);
	}

	.ancillary-status-dot.busy {
		background: var(--color-warning);
	}

	.item-name {
		font-weight: 500;
		color: var(--color-text);
	}

	.item-bead {
		font-size: 0.8rem;
		color: var(--color-text-secondary);
		font-family: var(--font-mono);
	}

	@media (min-width: 768px) {
		.mobile-only {
			display: none;
		}

		.mobile-overlay {
			display: none;
		}
	}
</style>
