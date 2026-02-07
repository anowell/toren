<script lang="ts">
import { torenStore } from '$lib/stores/toren';
import type { Segment } from '$lib/types/toren';

let showCreateModal = false;
let newSegmentName = '';
let selectedRoot = '';
let creating = false;
let error = '';

$: selectedRoot = ($torenStore.segmentRoots ?? [])[0] || '';

// Get set of segment names that have assignments
$: segmentsWithAssignments = new Set(
	($torenStore.assignments ?? []).map((a) => a.segment.toLowerCase()),
);

// Sort segments: those with assignments first, then alphabetically within each group
$: sortedSegments = [...($torenStore.segments ?? [])].sort((a, b) => {
	const aHasAssignment = segmentsWithAssignments.has(a.name.toLowerCase());
	const bHasAssignment = segmentsWithAssignments.has(b.name.toLowerCase());
	if (aHasAssignment && !bHasAssignment) return -1;
	if (!aHasAssignment && bHasAssignment) return 1;
	return a.name.localeCompare(b.name);
});

function selectSegment(segment: Segment) {
	torenStore.selectSegment(segment);
}

function openCreateModal() {
	showCreateModal = true;
	newSegmentName = '';
	error = '';
}

function closeCreateModal() {
	showCreateModal = false;
	newSegmentName = '';
	error = '';
}

async function handleCreateSegment() {
	if (!newSegmentName.trim()) {
		error = 'Segment name is required';
		return;
	}

	if (!selectedRoot) {
		error = 'No root directory available';
		return;
	}

	creating = true;
	error = '';

	try {
		const segment = await torenStore.createSegment(
			newSegmentName.trim(),
			selectedRoot,
			$torenStore.shipUrl,
		);
		torenStore.selectSegment(segment);
		closeCreateModal();
	} catch (err) {
		error = err instanceof Error ? err.message : 'Failed to create segment';
	} finally {
		creating = false;
	}
}

function getSegmentIcon(source: string) {
	switch (source) {
		case 'glob':
			return '📁';
		case 'path':
			return '📍';
		case 'root':
			return '✨';
		default:
			return '📂';
	}
}
</script>

<div class="segment-selector">
	<div class="header">
		<h2>Select Project</h2>
		{#if ($torenStore.segmentRoots ?? []).length > 0}
			<button class="create-btn" on:click={openCreateModal} aria-label="Create new segment">
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
					<line x1="12" y1="5" x2="12" y2="19"></line>
					<line x1="5" y1="12" x2="19" y2="12"></line>
				</svg>
			</button>
		{/if}
	</div>

	{#if $torenStore.loadingSegments}
		<div class="loading">Loading segments...</div>
	{:else if sortedSegments.length === 0}
		<div class="empty-state">
			<p>No segments found</p>
			{#if ($torenStore.segmentRoots ?? []).length > 0}
				<button class="create-segment-btn" on:click={openCreateModal}>
					Create New Project
				</button>
			{:else}
				<p class="help-text">Configure segment globs in toren.toml</p>
			{/if}
		</div>
	{:else}
		<div class="segment-list">
			{#each sortedSegments as segment (segment.path)}
				<button
					class="segment-card"
					class:selected={$torenStore.selectedSegment?.path === segment.path}
					on:click={() => selectSegment(segment)}
				>
					<div class="segment-icon">{getSegmentIcon(segment.source)}</div>
					<div class="segment-info">
						<div class="segment-name">{segment.name}</div>
						<div class="segment-path">{segment.path}</div>
					</div>
					{#if $torenStore.selectedSegment?.path === segment.path}
						<div class="selected-indicator">
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
								<polyline points="20 6 9 17 4 12"></polyline>
							</svg>
						</div>
					{/if}
				</button>
			{/each}
		</div>
	{/if}
</div>

{#if showCreateModal}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<div class="modal-overlay" on:click={closeCreateModal} role="presentation">
		<div class="modal" on:click|stopPropagation role="dialog" aria-labelledby="create-segment-title" tabindex="-1">
			<h3 id="create-segment-title">Create New Project</h3>

			<form on:submit|preventDefault={handleCreateSegment}>
				<div class="form-group">
					<label for="segment-name">Project Name</label>
					<input
						type="text"
						id="segment-name"
						bind:value={newSegmentName}
						placeholder="my-project"
						disabled={creating}
						autocomplete="off"
						required
					/>
				</div>

				{#if ($torenStore.segmentRoots ?? []).length > 1}
					<div class="form-group">
						<label for="segment-root">Root Directory</label>
						<select id="segment-root" bind:value={selectedRoot} disabled={creating}>
							{#each $torenStore.segmentRoots ?? [] as root}
								<option value={root}>{root}</option>
							{/each}
						</select>
					</div>
				{:else if ($torenStore.segmentRoots ?? []).length === 1}
					<div class="form-group">
						<span>Root Directory</span>
						<div class="read-only">{($torenStore.segmentRoots ?? [])[0]}</div>
					</div>
				{/if}

				{#if error}
					<div class="error">{error}</div>
				{/if}

				<div class="modal-actions">
					<button type="button" class="secondary" on:click={closeCreateModal} disabled={creating}>
						Cancel
					</button>
					<button type="submit" class="primary" disabled={creating || !newSegmentName.trim()}>
						{creating ? 'Creating...' : 'Create'}
					</button>
				</div>
			</form>
		</div>
	</div>
{/if}

<style>
	.segment-selector {
		display: flex;
		flex-direction: column;
		height: 100%;
		background: var(--color-bg);
	}

	.header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--spacing-md);
		border-bottom: 1px solid var(--color-border);
		background: var(--color-bg-secondary);
	}

	h2 {
		margin: 0;
		font-size: 1.25rem;
		color: var(--color-text);
	}

	.create-btn {
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

	.create-btn:hover {
		background: var(--color-primary-hover);
	}

	.loading {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		color: var(--color-text-secondary);
	}

	.empty-state {
		flex: 1;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		padding: var(--spacing-xl);
		text-align: center;
		gap: var(--spacing-md);
	}

	.empty-state p {
		margin: 0;
		color: var(--color-text-secondary);
	}

	.help-text {
		font-size: 0.85rem;
		font-family: var(--font-mono);
	}

	.create-segment-btn {
		padding: var(--spacing-md) var(--spacing-lg);
		background: var(--color-primary);
		color: white;
		border-radius: var(--radius-md);
		font-weight: 600;
		transition: background-color 0.2s;
	}

	.create-segment-btn:hover {
		background: var(--color-primary-hover);
	}

	.segment-list {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-sm);
		display: flex;
		flex-direction: column;
		gap: var(--spacing-sm);
	}

	.segment-card {
		display: flex;
		align-items: center;
		gap: var(--spacing-md);
		padding: var(--spacing-md);
		background: var(--color-bg-secondary);
		border: 2px solid var(--color-border);
		border-radius: var(--radius-lg);
		text-align: left;
		transition: all 0.2s;
		min-height: 72px;
		cursor: pointer;
	}

	.segment-card:hover {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.segment-card.selected {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.segment-icon {
		font-size: 2rem;
		flex-shrink: 0;
	}

	.segment-info {
		flex: 1;
		min-width: 0;
	}

	.segment-name {
		font-weight: 600;
		color: var(--color-text);
		margin-bottom: var(--spacing-xs);
		font-size: 1.05rem;
	}

	.segment-path {
		font-size: 0.85rem;
		color: var(--color-text-secondary);
		font-family: var(--font-mono);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.selected-indicator {
		color: var(--color-primary);
		flex-shrink: 0;
	}

	/* Modal */
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
		max-width: 400px;
		width: 100%;
	}

	.modal h3 {
		margin: 0 0 var(--spacing-lg) 0;
		color: var(--color-primary);
	}

	.form-group {
		margin-bottom: var(--spacing-md);
	}

	label {
		display: block;
		margin-bottom: var(--spacing-xs);
		color: var(--color-text);
		font-size: 0.9rem;
		font-weight: 500;
	}

	input,
	select {
		width: 100%;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		font-size: 1rem;
		transition: border-color 0.2s;
	}

	input:focus,
	select:focus {
		border-color: var(--color-primary);
	}

	input:disabled,
	select:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.read-only {
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text-secondary);
		font-family: var(--font-mono);
		font-size: 0.9rem;
	}

	.error {
		margin-bottom: var(--spacing-md);
		padding: var(--spacing-sm) var(--spacing-md);
		background: rgba(248, 113, 113, 0.1);
		border: 1px solid var(--color-error);
		border-radius: var(--radius-sm);
		color: var(--color-error);
		font-size: 0.9rem;
	}

	.modal-actions {
		display: flex;
		gap: var(--spacing-sm);
		margin-top: var(--spacing-lg);
	}

	.modal-actions button {
		flex: 1;
		padding: var(--spacing-md);
		border-radius: var(--radius-md);
		font-size: 1rem;
		font-weight: 600;
		transition: background-color 0.2s;
	}

	.modal-actions .primary {
		background: var(--color-primary);
		color: white;
	}

	.modal-actions .primary:hover:not(:disabled) {
		background: var(--color-primary-hover);
	}

	.modal-actions .secondary {
		background: var(--color-bg-tertiary);
		color: var(--color-text);
		border: 1px solid var(--color-border);
	}

	.modal-actions .secondary:hover:not(:disabled) {
		background: var(--color-bg);
	}

	.modal-actions button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	/* Mobile optimizations */
	@media (max-width: 768px) {
		.segment-card {
			min-height: 80px;
		}

		.segment-icon {
			font-size: 1.75rem;
		}
	}
</style>
