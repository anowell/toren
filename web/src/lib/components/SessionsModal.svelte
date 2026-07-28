<script lang="ts">
import { createEventDispatcher } from 'svelte';
import SessionList from '$lib/components/SessionList.svelte';
import type { AgentSession } from '$lib/types/toren';

// biome-ignore-start lint/style/useConst: svelte props are reassigned by the parent
/** The workspace's recorded sessions, newest first. */
export let sessions: AgentSession[] = [];
export let loading = false;
export let error: string | null = null;
/** The session being resumed right now, so its row can say so. */
export let busyId: string | null = null;
// biome-ignore-end lint/style/useConst: svelte props are reassigned by the parent

const dispatch = createEventDispatcher<{ close: null }>();
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="modal-overlay" on:click={() => dispatch('close', null)} role="presentation">
	<div class="modal" on:click|stopPropagation role="dialog" tabindex="-1">
		<h2>Resume a session</h2>
		<p class="subtitle">
			Each resume opens a new pane; the one it came from stays until you dismiss it.
		</p>

		<SessionList {sessions} {loading} {error} {busyId} on:resume />

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
