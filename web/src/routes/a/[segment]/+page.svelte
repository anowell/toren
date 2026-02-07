<script lang="ts">
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

let messageInput = '';
let showMobilePanel = false;
let sending = false;
let sendError: string | null = null;

function goToSegmentSelector() {
	torenStore.selectSegment(null);
	goto('/');
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
	closeMobilePanel();
}

function lookupAncillaryDisplayStatus(ancillaryId: string): 'busy' | 'ready' {
	const ancillary = $torenStore.ancillaries.find((a) => a.id === ancillaryId);
	if (!ancillary) return 'ready';
	return getAncillaryDisplayStatus(ancillary.status);
}

async function handleSendMessage() {
	if (!messageInput.trim() || sending) return;

	const content = messageInput.trim();
	const segment = $torenStore.selectedSegment?.name;
	if (!segment) return;

	sending = true;
	sendError = null;
	messageInput = '';

	try {
		const shipUrl = $torenStore.shipUrl;

		// 1. Create assignment (creates bead + workspace)
		const assignment = await torenStore.createAssignment(shipUrl, {
			prompt: content,
			segment,
		});

		// 2. Start the Claude agent work
		await torenStore.startWork(shipUrl, assignment.ancillary_id, assignment.id);

		// 3. Navigate to the ancillary's chat page
		const unit = assignment.ancillary_id.split(' ').pop()?.toLowerCase();
		goto(`/a/${$page.params.segment}/${unit}`);
	} catch (err) {
		sendError = err instanceof Error ? err.message : 'Failed to create ancillary';
		// Restore the message so user can retry
		messageInput = content;
	} finally {
		sending = false;
	}
}
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
	<div class="ancillary-indicator new">
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="14"
			height="14"
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
		<span class="hint">Will be assigned on first message</span>
	</div>

	<!-- Messages area -->
	<div class="chat-messages">
		<div class="empty-state">
			{#if sending}
				<div class="empty-icon spinning">+</div>
				<h2>Spinning up ancillary...</h2>
				<p>Creating workspace and starting agent</p>
			{:else}
				<div class="empty-icon">+</div>
				<h2>New Ancillary</h2>
				<p>Send a message to start a new task. An ancillary will be assigned automatically.</p>
				{#if sendError}
					<p class="error-text">{sendError}</p>
				{/if}
			{/if}
		</div>
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
				placeholder="Describe a task..."
				rows="1"
				disabled={sending}
				on:keydown={(e) => {
					if (e.key === 'Enter' && !e.shiftKey) {
						e.preventDefault();
						handleSendMessage();
					}
				}}
			></textarea>
			<button type="submit" disabled={!messageInput.trim() || sending} aria-label="Send message">
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
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<div class="mobile-overlay" on:click={closeMobilePanel} role="presentation">
		<div class="mobile-panel" on:click|stopPropagation role="dialog" tabindex="-1">
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
				<button class="mobile-item selected" on:click={navigateToNewAncillary}>
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
					{@const displayStatus = lookupAncillaryDisplayStatus(assignment.ancillary_id)}
					{@const beadStatus = getBeadDisplayStatus(assignment.status)}
					<button class="mobile-item" on:click={() => navigateToAncillary(assignment.ancillary_id)}>
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
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border-bottom: 1px solid var(--color-border);
		font-size: 0.85rem;
		color: var(--color-text);
	}

	.ancillary-indicator.new {
		color: var(--color-primary);
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
		color: var(--color-primary);
		border: 2px dashed var(--color-border);
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

	.error-text {
		color: var(--color-error);
		font-size: 0.85rem;
		margin-top: var(--spacing-sm);
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

	.mobile-item.selected .item-main {
		color: var(--color-primary);
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
