<script lang="ts">
	import { torenStore, messages } from '$lib/stores/toren';
	import type { ChatMessage } from '$lib/stores/toren';

	let messageInput = '';
	let chatContainer: HTMLElement;

	function addMessage(role: ChatMessage['role'], content: string) {
		torenStore.update((state) => ({
			...state,
			messages: [
				...state.messages,
				{
					id: crypto.randomUUID(),
					role,
					content,
					timestamp: new Date(),
				},
			],
		}));

		// Scroll to bottom
		setTimeout(() => {
			if (chatContainer) {
				chatContainer.scrollTop = chatContainer.scrollHeight;
			}
		}, 100);
	}

	function handleSendMessage() {
		if (!messageInput.trim()) return;

		const content = messageInput.trim();
		messageInput = '';

		// Add user message
		addMessage('user', content);

		// Add placeholder assistant message
		addMessage('assistant', 'Working on it...');

		// TODO: Integrate with actual ancillary/Claude API
		// For now, this is just a UI mockup
	}

	function formatTime(date: Date): string {
		return date.toLocaleTimeString('en-US', {
			hour: 'numeric',
			minute: '2-digit',
		});
	}
</script>

<div class="chat-interface">
	<div class="chat-header">
		<div class="header-content">
			<div class="header-left">
				<h1>Toren</h1>
				{#if $torenStore.selectedSegment}
					<div class="segment-badge">
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
							<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path>
						</svg>
						{$torenStore.selectedSegment.name}
					</div>
				{/if}
			</div>
			<div class="status">
				<span class="status-dot" class:connected={$torenStore.connected}></span>
				<span class="status-text">
					{$torenStore.connected ? 'Connected' : 'Disconnected'}
				</span>
			</div>
		</div>
	</div>

	<div class="chat-messages" bind:this={chatContainer}>
		{#if $messages.length === 0}
			<div class="empty-state">
				<div class="empty-icon">🚀</div>
				<h2>I am Toren. I am continuity.</h2>
				<p>Start a conversation to control your development environment.</p>
			</div>
		{:else}
			{#each $messages as message (message.id)}
				<div class="message" class:user={message.role === 'user'} class:assistant={message.role === 'assistant'}>
					<div class="message-header">
						<span class="message-role">
							{message.role === 'user' ? 'You' : 'Toren'}
						</span>
						<span class="message-time">{formatTime(message.timestamp)}</span>
					</div>
					<div class="message-content">
						{message.content}
					</div>
					{#if message.commandOutputs && message.commandOutputs.length > 0}
						<div class="command-outputs">
							{#each message.commandOutputs as output}
								<div class="output-line" class:stderr={output.type === 'Stderr'} class:error={output.type === 'Error'}>
									{output.line || output.message || ''}
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/each}
		{/if}
	</div>

	<div class="chat-input">
		<form on:submit|preventDefault={handleSendMessage}>
			<textarea
				bind:value={messageInput}
				placeholder="Type a message..."
				rows="1"
				on:keydown={(e) => {
					if (e.key === 'Enter' && !e.shiftKey) {
						e.preventDefault();
						handleSendMessage();
					}
				}}
			></textarea>
			<button type="submit" disabled={!messageInput.trim()} aria-label="Send message">
				<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
					<line x1="22" y1="2" x2="11" y2="13"></line>
					<polygon points="22 2 15 22 11 13 2 9 22 2"></polygon>
				</svg>
			</button>
		</form>
	</div>
</div>

<style>
	.chat-interface {
		display: flex;
		flex-direction: column;
		height: 100vh;
		background: var(--color-bg);
	}

	.chat-header {
		flex-shrink: 0;
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		padding: var(--spacing-md);
	}

	.header-content {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}

	.header-left {
		display: flex;
		align-items: center;
		gap: var(--spacing-md);
		flex-wrap: wrap;
	}

	h1 {
		margin: 0;
		font-size: 1.5rem;
		color: var(--color-primary);
	}

	.segment-badge {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-sm);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		font-size: 0.85rem;
		color: var(--color-text-secondary);
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
		transition: background-color 0.3s;
	}

	.status-dot.connected {
		background: var(--color-success);
	}

	.status-text {
		font-size: 0.85rem;
		color: var(--color-text-secondary);
	}

	.chat-messages {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-md);
		display: flex;
		flex-direction: column;
		gap: var(--spacing-md);
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		justify-content: center;
		align-items: center;
		text-align: center;
		gap: var(--spacing-md);
		padding: var(--spacing-xl);
	}

	.empty-icon {
		font-size: 4rem;
	}

	.empty-state h2 {
		margin: 0;
		color: var(--color-text);
	}

	.empty-state p {
		margin: 0;
		color: var(--color-text-secondary);
	}

	.message {
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
		padding: var(--spacing-md);
		border-radius: var(--radius-lg);
		max-width: 80%;
	}

	.message.user {
		align-self: flex-end;
		background: var(--color-primary);
		color: white;
	}

	.message.assistant {
		align-self: flex-start;
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
	}

	.message-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: var(--spacing-md);
	}

	.message-role {
		font-size: 0.85rem;
		font-weight: 600;
		opacity: 0.9;
	}

	.message-time {
		font-size: 0.75rem;
		opacity: 0.7;
	}

	.message-content {
		line-height: 1.5;
		white-space: pre-wrap;
		word-wrap: break-word;
	}

	.command-outputs {
		margin-top: var(--spacing-sm);
		padding: var(--spacing-sm);
		background: var(--color-bg-tertiary);
		border-radius: var(--radius-sm);
		font-family: var(--font-mono);
		font-size: 0.85rem;
		max-height: 200px;
		overflow-y: auto;
	}

	.output-line {
		padding: 2px 0;
		color: var(--color-text-secondary);
	}

	.output-line.stderr {
		color: var(--color-warning);
	}

	.output-line.error {
		color: var(--color-error);
	}

	.chat-input {
		flex-shrink: 0;
		background: var(--color-bg-secondary);
		border-top: 1px solid var(--color-border);
		padding: var(--spacing-md);
	}

	form {
		display: flex;
		gap: var(--spacing-sm);
		align-items: flex-end;
	}

	textarea {
		flex: 1;
		min-height: 44px;
		max-height: 200px;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		resize: none;
		font-size: 1rem;
		line-height: 1.5;
		transition: border-color 0.2s;
	}

	textarea:focus {
		border-color: var(--color-primary);
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
		transition: background-color 0.2s;
	}

	button[type='submit']:hover:not(:disabled) {
		background: var(--color-primary-hover);
	}

	button[type='submit']:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	/* Mobile optimizations */
	@media (max-width: 768px) {
		.message {
			max-width: 90%;
		}

		.chat-input {
			padding: var(--spacing-sm);
		}
	}
</style>
