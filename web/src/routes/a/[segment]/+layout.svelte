<script lang="ts">
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import BeadStatusIcon from '$lib/components/BeadStatusIcon.svelte';
import {
	getAncillaryDisplayStatus,
	getBeadDisplayStatus,
	segmentAssignments,
	stripBeadPrefix,
	torenStore,
} from '$lib/stores/toren';

// Load assignments when authenticated
let assignmentsLoaded = false;
$: if ($torenStore.authenticated && $torenStore.shipUrl && !assignmentsLoaded) {
	assignmentsLoaded = true;
	torenStore.loadAssignments($torenStore.shipUrl);
}

// Sync segment from URL to store
$: {
	const segmentName = $page.params.segment;
	if (segmentName && $torenStore.segments.length > 0) {
		const segment = $torenStore.segments.find(
			(s) => s.name.toLowerCase() === segmentName.toLowerCase(),
		);
		if (segment && $torenStore.selectedSegment?.name !== segment.name) {
			torenStore.selectSegment(segment);
		}
	}
}

// Get current unit from URL (if any)
$: currentUnit = $page.params.unit || null;

// Check if this is the "new ancillary" view
$: isNewAncillary = !currentUnit;

function navigateToAncillary(ancillaryId: string) {
	// Extract unit number from ancillary ID (e.g., "Toren One" -> "one")
	const parts = ancillaryId.split(' ');
	const unit = parts[parts.length - 1].toLowerCase();
	goto(`/a/${$page.params.segment}/${unit}`);
}

function navigateToNewAncillary() {
	goto(`/a/${$page.params.segment}`);
}

function lookupAgentActivity(assignment: import('$lib/types/toren').Assignment): 'busy' | 'ready' {
	// Prefer composite signal from API
	if (assignment.agent_activity === 'busy') return 'busy';
	if (assignment.agent_activity === 'idle') return 'ready';
	// Fallback to ancillary status
	const ancillary = $torenStore.ancillaries.find((a) => a.id === assignment.ancillary_id);
	if (!ancillary) return 'ready';
	return getAncillaryDisplayStatus(ancillary.status);
}
</script>

<div class="ancillary-layout">
	<!-- Desktop sidebar -->
	<aside class="desktop-sidebar">
		<div class="panel-header">
			<h3>Ancillaries</h3>
			<span class="count">{$segmentAssignments.length}</span>
		</div>

		<div class="ancillary-list">
			<!-- New Ancillary option -->
			<button
				class="ancillary-card new-ancillary"
				class:selected={isNewAncillary}
				on:click={navigateToNewAncillary}
			>
				<div class="card-header">
					<span class="ancillary-name">
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
						New Ancillary
					</span>
				</div>
			</button>

			{#each $segmentAssignments as assignment (assignment.id)}
				{@const unitName = assignment.ancillary_id.split(' ').pop()?.toLowerCase()}
				{@const agentStatus = lookupAgentActivity(assignment)}
				{@const beadStatus = getBeadDisplayStatus(assignment)}
				<button
					class="ancillary-card"
					class:selected={currentUnit === unitName}
					on:click={() => navigateToAncillary(assignment.ancillary_id)}
				>
					<div class="card-header">
						<span class="ancillary-status-dot" class:busy={agentStatus === 'busy'} class:ready={agentStatus === 'ready'}></span>
						<span class="ancillary-name">{assignment.ancillary_id}</span>
						{#if assignment.has_changes}<span class="changes-indicator" title="Has uncommitted changes">*</span>{/if}
					</div>
					<div class="card-body">
						<BeadStatusIcon status={beadStatus} />
						<span class="bead-label">{stripBeadPrefix(assignment.bead_id)}{#if assignment.bead_title}: {assignment.bead_title}{/if}</span>
					</div>
					{#if assignment.bead_assignee}
						<div class="card-footer">
							<span class="assignee-badge">@{assignment.bead_assignee}</span>
						</div>
					{/if}
				</button>
			{/each}
		</div>
	</aside>

	<!-- Main content area -->
	<main class="main-content">
		<slot />
	</main>
</div>

<style>
	.ancillary-layout {
		display: flex;
		height: 100vh;
		width: 100%;
		overflow: hidden;
	}

	.desktop-sidebar {
		display: none;
		flex-direction: column;
		width: 260px;
		flex-shrink: 0;
		background: var(--color-bg-secondary);
		border-right: 1px solid var(--color-border);
		height: 100%;
		overflow: hidden;
	}

	.panel-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--spacing-md);
		border-bottom: 1px solid var(--color-border);
	}

	.panel-header h3 {
		margin: 0;
		font-size: 0.85rem;
		font-weight: 600;
		color: var(--color-text-secondary);
		text-transform: uppercase;
		letter-spacing: 0.05em;
	}

	.count {
		display: flex;
		align-items: center;
		justify-content: center;
		min-width: 22px;
		height: 22px;
		padding: 0 var(--spacing-xs);
		background: var(--color-bg-tertiary);
		border-radius: var(--radius-sm);
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--color-text-secondary);
	}

	.ancillary-list {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-sm);
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
	}

	.ancillary-card {
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		text-align: left;
		cursor: pointer;
		transition: all 0.15s ease;
	}

	.ancillary-card:hover {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.ancillary-card.selected {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.ancillary-card.new-ancillary {
		border-style: dashed;
	}

	.ancillary-card.new-ancillary .ancillary-name {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		color: var(--color-primary);
	}

	.card-header {
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

	.ancillary-name {
		font-weight: 500;
		color: var(--color-text);
		font-size: 0.9rem;
	}

	.card-body {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		color: var(--color-text-secondary);
	}

	.bead-label {
		font-size: 0.8rem;
		color: var(--color-text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.changes-indicator {
		color: var(--color-warning);
		font-weight: 700;
		font-size: 0.9rem;
		margin-left: auto;
	}

	.card-footer {
		display: flex;
		align-items: center;
	}

	.assignee-badge {
		font-size: 0.75rem;
		color: var(--color-text-secondary);
		background: var(--color-bg-tertiary);
		padding: 1px var(--spacing-xs);
		border-radius: var(--radius-sm);
	}

	.main-content {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		height: 100%;
	}

	@media (min-width: 768px) {
		.desktop-sidebar {
			display: flex;
		}
	}
</style>
