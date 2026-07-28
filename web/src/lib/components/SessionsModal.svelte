<script lang="ts">
import { createEventDispatcher } from 'svelte';
import type { AgentSession } from '$lib/types/toren';

// biome-ignore-start lint/style/useConst: svelte props are reassigned by the parent
/** The workspace's recorded sessions, newest first. */
export let sessions: AgentSession[] = [];
export let loading = false;
export let error: string | null = null;
/** The session being resumed right now, so its row can say so. */
export let busyId: string | null = null;
// biome-ignore-end lint/style/useConst: svelte props are reassigned by the parent

const dispatch = createEventDispatcher<{
	resume: { session: AgentSession };
	close: null;
}>();

/** A session with no id was never named by its agent, so there is nothing to resume it by. */
function resumable(session: AgentSession): boolean {
	return Boolean(session.id);
}

function label(session: AgentSession): string {
	return session.title || session.id || 'unnamed session';
}

function when(session: AgentSession): string {
	const stamp = session.started_at;
	if (!stamp) return '';
	const at = new Date(stamp);
	return Number.isNaN(at.getTime()) ? stamp : at.toLocaleString();
}
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="modal-overlay" on:click={() => dispatch('close', null)} role="presentation">
	<div class="modal" on:click|stopPropagation role="dialog" tabindex="-1">
		<h2>Resume a session</h2>
		<p class="subtitle">
			Each resume opens a new pane; the one it came from stays until you dismiss it.
		</p>

		{#if error}
			<div class="error">{error}</div>
		{/if}

		{#if loading}
			<div class="empty">Loading…</div>
		{:else if sessions.length === 0}
			<div class="empty">No agent sessions recorded in this workspace yet.</div>
		{:else}
			<div class="session-list">
				{#each sessions as session, index (session.id ?? index)}
					<button
						class="session-row"
						disabled={!resumable(session) || busyId !== null}
						on:click={() => dispatch('resume', { session })}
						title={session.id ?? 'This session was never named by its agent'}
					>
						<span class="row-main">
							<span class="row-title">{label(session)}</span>
							<span class="badge mono">{session.agent}</span>
							{#if session.exit !== undefined && session.exit !== null}
								<span class="badge" class:failed={session.exit !== 0}>exit {session.exit}</span>
							{:else if !session.ended_at}
								<span class="badge running">open</span>
							{/if}
						</span>
						<span class="row-meta">
							{when(session)}
							{#if session.task}· {session.task}{/if}
							{#if busyId && busyId === session.id}· resuming…{/if}
						</span>
					</button>
				{/each}
			</div>
		{/if}

		<button class="dismiss" on:click={() => dispatch('close', null)}>Close</button>
	</div>
</div>

<style>
	.modal-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.8);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
		padding: var(--spacing-md);
	}

	.modal {
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
		padding: var(--spacing-xl);
		max-width: 520px;
		width: 100%;
	}

	h2 {
		margin: 0 0 var(--spacing-sm) 0;
		color: var(--color-primary);
		font-size: 1.1rem;
	}

	.subtitle {
		margin: 0 0 var(--spacing-lg) 0;
		color: var(--color-text-secondary);
		font-size: 0.85rem;
	}

	.session-list {
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
		max-height: 50vh;
		overflow-y: auto;
	}

	.session-row {
		display: flex;
		flex-direction: column;
		gap: 2px;
		width: 100%;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text);
		text-align: left;
		cursor: pointer;
	}

	.session-row:hover:not(:disabled) {
		border-color: var(--color-primary);
	}

	.session-row:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.row-main {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
	}

	.row-title {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.9rem;
	}

	.row-meta {
		color: var(--color-text-secondary);
		font-size: 0.75rem;
	}

	.badge {
		font-size: 0.65rem;
		padding: 1px 6px;
		border-radius: var(--radius-sm);
		background: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
		flex-shrink: 0;
	}

	.badge.failed {
		background: var(--color-error);
		color: white;
	}

	.badge.running {
		background: var(--color-warning);
		color: var(--color-bg);
	}

	.mono {
		font-family: var(--font-mono);
	}

	.empty {
		padding: var(--spacing-lg);
		text-align: center;
		color: var(--color-text-secondary);
		font-size: 0.85rem;
	}

	.error {
		margin-bottom: var(--spacing-md);
		padding: var(--spacing-sm) var(--spacing-md);
		background: rgba(248, 113, 113, 0.1);
		border: 1px solid var(--color-error);
		border-radius: var(--radius-sm);
		color: var(--color-error);
		font-size: 0.85rem;
	}

	.dismiss {
		width: 100%;
		margin-top: var(--spacing-lg);
		padding: var(--spacing-sm);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text);
		font-size: 0.9rem;
		cursor: pointer;
	}

	.dismiss:hover {
		border-color: var(--color-primary);
	}
</style>
