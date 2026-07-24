<script lang="ts">
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import TaskStatusIcon from '$lib/components/TaskStatusIcon.svelte';
import {
	getTaskDisplayStatus,
	getWorkspaceDisplayStatus,
	primaryTask,
	segmentWorkspaces,
	stripTaskPrefix,
	torenStore,
} from '$lib/stores/toren';
import type { WorkspaceView } from '$lib/types/toren';

// Load workspaces when authenticated
let workspacesLoaded = false;
$: if ($torenStore.authenticated && $torenStore.shipUrl && !workspacesLoaded) {
	workspacesLoaded = true;
	torenStore.loadWorkspaces($torenStore.shipUrl);
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

// Current workspace name from URL (if any)
$: currentName = $page.params.name || null;

function navigateToWorkspace(ws: WorkspaceView) {
	goto(`/a/${$page.params.segment}/${encodeURIComponent(ws.name)}`);
}
</script>

<div class="workspace-layout">
	<!-- Desktop sidebar -->
	<aside class="desktop-sidebar">
		<div class="panel-header">
			<h3>Workspaces</h3>
			<span class="count">{$segmentWorkspaces.length}</span>
		</div>

		<div class="workspace-list">
			{#each $segmentWorkspaces as ws (ws.name)}
				{@const agentStatus = getWorkspaceDisplayStatus(ws)}
				{@const task = primaryTask(ws)}
				<button
					class="workspace-card"
					class:selected={currentName === ws.name}
					on:click={() => navigateToWorkspace(ws)}
				>
					<div class="card-header">
						<span class="workspace-status-dot" class:busy={agentStatus === 'busy'} class:ready={agentStatus === 'ready'}></span>
						<span class="workspace-name">{ws.name}</span>
						{#if ws.sets.changes.length > 0}<span class="changes-indicator" title="Has uncommitted changes">*</span>{/if}
					</div>
					{#if task}
						<div class="card-body">
							<TaskStatusIcon status={getTaskDisplayStatus(task)} />
							<span class="task-label">{stripTaskPrefix(task.id)}{#if task.title}: {task.title}{/if}</span>
						</div>
					{:else if ws.title}
						<div class="card-body">
							<span class="task-label">{ws.title}</span>
						</div>
					{/if}
					{#if task?.assignee}
						<div class="card-footer">
							<span class="assignee-badge">@{task.assignee}</span>
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
	.workspace-layout {
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

	.workspace-list {
		flex: 1;
		overflow-y: auto;
		padding: var(--spacing-sm);
		display: flex;
		flex-direction: column;
		gap: var(--spacing-xs);
	}

	.workspace-card {
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

	.workspace-card:hover {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.workspace-card.selected {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
	}

	.card-header {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
	}

	.workspace-status-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex-shrink: 0;
	}

	.workspace-status-dot.ready {
		background: var(--color-success);
	}

	.workspace-status-dot.busy {
		background: var(--color-warning);
	}

	.workspace-name {
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

	.task-label {
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
