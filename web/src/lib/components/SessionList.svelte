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

const dispatch = createEventDispatcher<{ resume: { session: AgentSession } }>();

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
			<div class="session-row">
				<div class="row-text">
					<span class="row-main">
						<span class="row-title" title={session.id ?? 'This session was never named by its agent'}>
							{label(session)}
						</span>
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
					</span>
				</div>
				<button
					class="resume"
					disabled={!resumable(session) || busyId !== null}
					on:click={() => dispatch('resume', { session })}
				>
					{busyId && busyId === session.id ? 'Resuming…' : 'Resume'}
				</button>
			</div>
		{/each}
	</div>
{/if}

<style>
	.session-list {
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
		max-height: 50vh;
		overflow-y: auto;
	}

	.session-row {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text);
	}

	.row-text {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
		flex: 1;
	}

	.row-main {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		min-width: 0;
	}

	.row-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-size: 0.9rem;
	}

	.row-meta {
		color: var(--color-text-secondary);
		font-size: 0.75rem;
	}

	.resume {
		flex-shrink: 0;
		padding: 2px 10px;
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		color: var(--color-primary);
		font-size: 0.75rem;
		font-weight: 600;
	}

	.resume:hover:not(:disabled) {
		border-color: var(--color-primary);
	}

	.resume:disabled {
		opacity: 0.5;
		cursor: not-allowed;
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
		padding: var(--spacing-md);
		text-align: center;
		color: var(--color-text-secondary);
		font-size: 0.85rem;
	}

	.error {
		margin-bottom: var(--spacing-sm);
		padding: var(--spacing-sm) var(--spacing-md);
		background: rgba(248, 113, 113, 0.1);
		border: 1px solid var(--color-error);
		border-radius: var(--radius-sm);
		color: var(--color-error);
		font-size: 0.85rem;
	}
</style>
