<script lang="ts">
import { onDestroy } from 'svelte';
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import AgentTerminal from '$lib/components/AgentTerminal.svelte';
import SegmentDropdown from '$lib/components/SegmentDropdown.svelte';
import TaskStatusIcon from '$lib/components/TaskStatusIcon.svelte';
import { connectionStore } from '$lib/stores/connection';
import {
	defaultWindowName,
	getTaskDisplayStatus,
	getWorkspaceDisplayStatus,
	primaryTask,
	segmentWorkspaces,
	stripTaskPrefix,
	torenStore,
} from '$lib/stores/toren';
import type { SessionInfo, WorkspaceView } from '$lib/types/toren';

let messageInput = '';
let showMobilePanel = false;
let showDetails = false;

/** Typed structurally so biome doesn't see the import as type-only. */
let terminal: { sendLine(text: string): void; interrupt(): void } | null = null;
let paneStatus = 'connecting';
let paneSession: string | null = null;
let wsError: string | null = null;

function goToSegmentSelector() {
	torenStore.selectSegment(null);
	goto('/');
}

// Get current workspace based on name param
$: currentWorkspace = $segmentWorkspaces.find((w) => w.name === $page.params.name);

$: nameParam = $page.params.name ?? '';

$: sessions = currentWorkspace?.sets.sessions ?? [];

// The window ("session" in the UI) the terminal is currently attached to. Tracked separately from
// the workspace so switching rows just re-points the pane bridge.
let selectedWindow: string | null = null;
// Which workspace `selectedWindow` was chosen for, so navigating between workspaces re-defaults it.
let selectedFor: string | null = null;

// Default to the daemon's default window on first view of a workspace, and recover to it if the
// currently-selected window disappears (e.g. the agent was stopped) after a refresh.
$: if (currentWorkspace) syncSelection(currentWorkspace);

function syncSelection(ws: WorkspaceView) {
	const list = ws.sets.sessions;
	if (selectedFor !== ws.name) {
		selectedFor = ws.name;
		selectedWindow = defaultWindowName(list);
	} else if (selectedWindow && !list.some((s) => s.window === selectedWindow)) {
		selectedWindow = defaultWindowName(list);
	}
}

function selectWindow(window: string) {
	if (!currentWorkspace) return;
	selectedFor = currentWorkspace.name;
	selectedWindow = window;
}

/** Prefer the agent's activity string over the raw status for the agent row. */
function sessionLabel(s: SessionInfo): string {
	return s.agent_activity || s.status;
}

// Gated on auth: the pane bridge is only opened once the main connection is up. When a specific
// window is selected we attach to it directly; otherwise we let the daemon pick the default.
$: terminalUrl =
	currentWorkspace && $torenStore.authenticated
		? `${$torenStore.shipUrl.replace(/^http/, 'ws')}/ws/workspaces/${encodeURIComponent(currentWorkspace.segment)}/${encodeURIComponent(currentWorkspace.name)}${
				selectedWindow ? `/${encodeURIComponent(selectedWindow)}` : ''
			}`
		: null;

$: if (terminalUrl) {
	paneStatus = 'connecting';
	paneSession = null;
	wsError = null;
}

$: displayStatus = currentWorkspace ? getWorkspaceDisplayStatus(currentWorkspace) : 'ready';
$: task = currentWorkspace ? primaryTask(currentWorkspace) : null;
$: sets = currentWorkspace?.sets;

function handleTerminalStatus(event: CustomEvent<{ status: string; session?: string }>) {
	paneStatus = event.detail.status;
	if (event.detail.session) paneSession = event.detail.session;
	if (paneStatus === 'attached') wsError = null;
}

function handleTerminalError(event: CustomEvent<{ message: string }>) {
	wsError = event.detail.message;
}

onDestroy(() => {
	terminal = null;
});

/** Redundant with the terminal on desktop, but the terminal is awkward to type into on a phone. */
function handleSendMessage() {
	const content = messageInput.trim();
	if (!content || !terminal) return;
	messageInput = '';
	terminal.sendLine(content);
}

function handleInterrupt() {
	terminal?.interrupt();
}

let lifecycleLoading = false;
let lifecycleError: string | null = null;
let shellLoading = false;

async function refreshCurrent(): Promise<WorkspaceView | null> {
	if (!currentWorkspace) return null;
	return torenStore.refreshWorkspace(
		$torenStore.shipUrl,
		currentWorkspace.segment,
		currentWorkspace.name,
	);
}

async function handleStart(resume: boolean) {
	if (!currentWorkspace || lifecycleLoading) return;
	lifecycleLoading = true;
	lifecycleError = null;
	try {
		await torenStore.startWorkspace(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
			{ resume },
		);
		await refreshCurrent();
		// The agent window now exists; attach to it.
		selectWindow('agent');
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to start';
	} finally {
		lifecycleLoading = false;
	}
}

async function handleStop() {
	if (!currentWorkspace || lifecycleLoading) return;
	lifecycleLoading = true;
	lifecycleError = null;
	try {
		await torenStore.stopWorkspace(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
		);
		const ws = await refreshCurrent();
		// The agent window is gone; fall back to a shell window if one is still around.
		const list = ws?.sets.sessions ?? [];
		const fallback = defaultWindowName(list.filter((s) => s.window !== 'agent'));
		if (fallback) selectWindow(fallback);
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to stop';
	} finally {
		lifecycleLoading = false;
	}
}

async function handleNewShell() {
	if (!currentWorkspace || shellLoading) return;
	shellLoading = true;
	lifecycleError = null;
	try {
		const window = await torenStore.startWorkspaceShell(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
		);
		await refreshCurrent();
		selectWindow(window);
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to open shell';
	} finally {
		shellLoading = false;
	}
}

function toggleMobilePanel() {
	showMobilePanel = !showMobilePanel;
}

function closeMobilePanel() {
	showMobilePanel = false;
}

function navigateToWorkspace(ws: WorkspaceView) {
	goto(`/a/${$page.params.segment}/${encodeURIComponent(ws.name)}`);
	closeMobilePanel();
}

$: attached = paneStatus === 'attached';
$: paneEnded = paneStatus === 'ended';
$: isWorking = displayStatus === 'busy';
</script>

<div class="workspace-view">
	<!-- Header -->
	<header class="workspace-header">
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

	<!-- Workspace indicator -->
	{#if currentWorkspace}
		<div class="workspace-indicator">
			<span class="workspace-name">{currentWorkspace.name}</span>
			<span class="separator">·</span>
			{#if task}
				<TaskStatusIcon status={getTaskDisplayStatus(task)} />
				<span class="task-label">{stripTaskPrefix(task.id)}{#if task.title}: {task.title}{/if}</span>
			{:else if currentWorkspace.title}
				<span class="task-label">{currentWorkspace.title}</span>
			{:else}
				<span class="task-label">{currentWorkspace.path}</span>
			{/if}
			<div class="indicator-actions">
				{#if isWorking}
					<button class="action-btn stop" on:click={handleStop} disabled={lifecycleLoading} title="Stop agent">Stop</button>
				{:else}
					<button class="action-btn start" on:click={() => handleStart(false)} disabled={lifecycleLoading} title="Start agent">Start</button>
					<button class="action-btn resume" on:click={() => handleStart(true)} disabled={lifecycleLoading} title="Resume agent">Resume</button>
				{/if}
				{#if lifecycleError}
					<span class="lifecycle-error">{lifecycleError}</span>
				{/if}
			</div>
		</div>

		<!-- Sessions (rmux windows): click to attach the terminal to that window -->
		<div class="sessions-bar">
			{#each sessions as s (s.window)}
				<button
					class="session-chip"
					class:active={s.window === selectedWindow}
					on:click={() => selectWindow(s.window)}
					title={s.command}
				>
					<span class="chip-dot status-{s.status}"></span>
					<span class="chip-name mono">{s.window}</span>
					<span class="chip-status">{sessionLabel(s)}</span>
					<span class="chip-cmd mono">{s.command}</span>
				</button>
			{/each}
			<button
				class="session-chip new-shell"
				on:click={handleNewShell}
				disabled={shellLoading}
				title="Open a new shell window"
			>
				{shellLoading ? 'Opening…' : '+ New shell'}
			</button>
		</div>

		<!-- Sets summary / details toggle -->
		{#if sets}
			<button class="sets-summary" on:click={() => (showDetails = !showDetails)}>
				<span>{sets.sessions.length} session{sets.sessions.length === 1 ? '' : 's'}</span>
				<span>{sets.changes.length} change{sets.changes.length === 1 ? '' : 's'}</span>
				<span>{sets.prs.length} PR{sets.prs.length === 1 ? '' : 's'}</span>
				<span>{sets.tasks.length} task{sets.tasks.length === 1 ? '' : 's'}</span>
				<span class="sets-chevron">{showDetails ? '▾' : '▸'}</span>
			</button>
			{#if showDetails}
				<div class="sets-details">
					{#if sets.sessions.length > 0}
						<section>
							<h4>Sessions</h4>
							{#each sets.sessions as s (s.window)}
								<div class="set-row">
									<span class="badge status-{s.status}">{s.status}</span>
									<span class="mono">{s.window}</span>
									<span class="dim mono">{s.command}</span>
									{#if s.agent_activity}<span class="dim">· {s.agent_activity}</span>{/if}
								</div>
							{/each}
						</section>
					{/if}
					{#if sets.branches.length > 0}
						<section>
							<h4>Branches</h4>
							<div class="set-row wrap">
								{#each sets.branches as b (b)}<span class="badge mono">{b}</span>{/each}
							</div>
						</section>
					{/if}
					{#if sets.changes.length > 0}
						<section>
							<h4>Changes</h4>
							{#each sets.changes as c (c.id)}
								<div class="set-row">
									<span class="mono dim">{stripTaskPrefix(c.id)}</span>
									<span>{c.summary}</span>
								</div>
							{/each}
						</section>
					{/if}
					{#if sets.prs.length > 0}
						<section>
							<h4>PRs{#if sets.prs_age} <span class="dim">({sets.prs_age})</span>{/if}</h4>
							{#each sets.prs as pr (pr.id)}
								<div class="set-row">
									<a href={pr.url} target="_blank" rel="noreferrer" class="mono">{pr.id}</a>
									<span class="mono dim">{pr.branch}</span>
									<span class="badge">{pr.state}</span>
									{#if pr.ci}<span class="badge">{pr.ci}</span>{/if}
								</div>
							{/each}
						</section>
					{/if}
					{#if sets.tasks.length > 0}
						<section>
							<h4>Tasks</h4>
							{#each sets.tasks as t (t.link)}
								<div class="set-row">
									<TaskStatusIcon status={getTaskDisplayStatus(t)} />
									{#if t.url}
										<a href={t.url} target="_blank" rel="noreferrer" class="mono">{stripTaskPrefix(t.id)}</a>
									{:else}
										<span class="mono">{stripTaskPrefix(t.id)}</span>
									{/if}
									{#if t.title}<span>{t.title}</span>{/if}
									{#if t.status}<span class="dim">· {t.status}</span>{/if}
									{#if t.assignee}<span class="dim">· @{t.assignee}</span>{/if}
									{#if t.error}<span class="lifecycle-error">{t.error}</span>{/if}
								</div>
							{/each}
						</section>
					{/if}
				</div>
			{/if}
		{/if}
	{:else}
		<div class="workspace-indicator not-found">
			<span>{$torenStore.selectedSegment?.name} / {nameParam}</span>
			<span class="hint">Workspace not found</span>
		</div>
	{/if}

	<!-- Agent terminal -->
	<div class="terminal-pane">
		{#if !currentWorkspace}
			<div class="empty-state">
				<div class="empty-icon">?</div>
				<h2>Workspace Not Found</h2>
				<p>No workspace named "{nameParam}" in this segment.</p>
			</div>
		{:else}
			{#if wsError}
				<div class="terminal-banner error">{wsError}</div>
			{:else if paneEnded}
				<div class="terminal-banner">
					Agent session ended. Start or resume to launch a new one; the transcript above is what it left behind.
				</div>
			{:else if !attached}
				<div class="terminal-banner">Attaching to {currentWorkspace.name}{selectedWindow ? ` · ${selectedWindow}` : ''}...</div>
			{:else if paneSession}
				<div class="terminal-banner">
					Attached to <code>{paneSession}</code> — <code>rmux attach -t {paneSession}</code> for the same pane in a terminal
				</div>
			{/if}
			<AgentTerminal
				bind:this={terminal}
				url={terminalUrl}
				on:status={handleTerminalStatus}
				on:error={handleTerminalError}
			/>
		{/if}
	</div>

	<!-- Input area -->
	<div class="workspace-input">
		<button class="panel-toggle mobile-only" on:click={toggleMobilePanel} aria-label="View workspaces">
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
			{#if $segmentWorkspaces.length > 0}
				<span class="badge">{$segmentWorkspaces.length}</span>
			{/if}
		</button>
		<form on:submit|preventDefault={handleSendMessage}>
			<textarea
				bind:value={messageInput}
				placeholder="Type a line into the terminal..."
				rows="1"
				disabled={!attached}
				on:keydown={(e) => {
					if (e.key === 'Enter' && !e.shiftKey) {
						e.preventDefault();
						handleSendMessage();
					}
				}}
			></textarea>
			<button type="submit" disabled={!messageInput.trim() || !attached} aria-label="Send line">
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
				<h3>Workspaces</h3>
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
				{#each $segmentWorkspaces as ws (ws.name)}
					{@const agentStatus = getWorkspaceDisplayStatus(ws)}
					{@const wsTask = primaryTask(ws)}
					<button
						class="mobile-item"
						class:selected={ws.name === $page.params.name}
						on:click={() => navigateToWorkspace(ws)}
					>
						<div class="item-main">
							<span class="workspace-status-dot" class:busy={agentStatus === 'busy'} class:ready={agentStatus === 'ready'}></span>
							<span class="item-name">{ws.name}</span>
						</div>
						{#if wsTask}
							<span class="item-task"><TaskStatusIcon status={getTaskDisplayStatus(wsTask)} /> {stripTaskPrefix(wsTask.id)}{#if wsTask.title}: {wsTask.title}{/if}</span>
						{:else if ws.title}
							<span class="item-task">{ws.title}</span>
						{/if}
					</button>
				{/each}
			</div>
		</div>
	</div>
{/if}

<style>
	.workspace-view {
		display: flex;
		flex-direction: column;
		height: 100%;
		width: 100%;
		background: var(--color-bg);
	}

	/* Header */
	.workspace-header {
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

	/* Workspace indicator */
	.workspace-indicator {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border-bottom: 1px solid var(--color-border);
		font-size: 0.85rem;
		flex-shrink: 0;
	}

	.workspace-indicator.not-found {
		color: var(--color-text-secondary);
	}

	.workspace-name {
		font-weight: 600;
		color: var(--color-text);
	}

	.separator {
		color: var(--color-text-secondary);
	}

	.task-label {
		color: var(--color-text-secondary);
		font-size: 0.8rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.indicator-actions {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
	}

	.action-btn {
		padding: 2px 10px;
		border-radius: var(--radius-sm);
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
		cursor: pointer;
		border: 1px solid transparent;
	}

	.action-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.action-btn.start {
		background: var(--color-success);
		color: var(--color-bg);
	}

	.action-btn.start:hover:not(:disabled) {
		opacity: 0.85;
	}

	.action-btn.stop {
		background: none;
		color: var(--color-error);
		border-color: var(--color-error);
	}

	.action-btn.stop:hover:not(:disabled) {
		background: var(--color-error);
		color: white;
	}

	.action-btn.resume {
		background: var(--color-primary);
		color: white;
	}

	.action-btn.resume:hover:not(:disabled) {
		opacity: 0.85;
	}

	.lifecycle-error {
		font-size: 0.7rem;
		color: var(--color-error);
	}

	.workspace-indicator .hint {
		color: var(--color-text-secondary);
		font-size: 0.75rem;
		margin-left: auto;
	}

	/* Sets summary / details */
	.sets-summary {
		display: flex;
		align-items: center;
		gap: var(--spacing-md);
		width: 100%;
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-secondary);
		border: none;
		border-bottom: 1px solid var(--color-border);
		color: var(--color-text-secondary);
		font-size: 0.75rem;
		cursor: pointer;
		text-align: left;
		flex-shrink: 0;
	}

	.sets-summary:hover {
		color: var(--color-text);
	}

	.sets-chevron {
		margin-left: auto;
	}

	.sets-details {
		flex-shrink: 0;
		max-height: 40vh;
		overflow-y: auto;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		font-size: 0.8rem;
	}

	.sets-details section {
		margin-bottom: var(--spacing-sm);
	}

	.sets-details h4 {
		margin: 0 0 var(--spacing-xs) 0;
		font-size: 0.7rem;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: var(--color-text-secondary);
	}

	.set-row {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: 2px 0;
		color: var(--color-text);
		overflow: hidden;
	}

	.set-row.wrap {
		flex-wrap: wrap;
	}

	.set-row a {
		color: var(--color-primary);
		text-decoration: none;
	}

	.set-row a:hover {
		text-decoration: underline;
	}

	.mono {
		font-family: var(--font-mono);
	}

	.dim {
		color: var(--color-text-secondary);
	}

	.badge {
		font-size: 0.65rem;
		padding: 1px 6px;
		border-radius: var(--radius-sm);
		background: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
	}

	.badge.status-running {
		background: var(--color-warning);
		color: var(--color-bg);
	}

	.badge.status-idle {
		background: var(--color-success);
		color: var(--color-bg);
	}

	.badge.status-exited {
		background: var(--color-border);
		color: var(--color-text-secondary);
	}

	/* Sessions bar (rmux windows) */
	.sessions-bar {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		overflow-x: auto;
		flex-shrink: 0;
	}

	.session-chip {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		flex-shrink: 0;
		padding: 3px 10px;
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text-secondary);
		font-size: 0.75rem;
		cursor: pointer;
		white-space: nowrap;
	}

	.session-chip:hover:not(:disabled) {
		border-color: var(--color-primary);
		color: var(--color-text);
	}

	.session-chip.active {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
		color: var(--color-text);
	}

	.session-chip:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.chip-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		flex-shrink: 0;
		background: var(--color-text-secondary);
	}

	.chip-dot.status-running {
		background: var(--color-warning);
	}

	.chip-dot.status-idle {
		background: var(--color-success);
	}

	.chip-dot.status-exited {
		background: var(--color-border);
	}

	.chip-name {
		font-weight: 600;
		color: var(--color-text);
	}

	.chip-status {
		color: var(--color-text-secondary);
	}

	.chip-cmd {
		color: var(--color-text-secondary);
		max-width: 14ch;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.session-chip.new-shell {
		font-weight: 600;
		color: var(--color-primary);
	}

	/* Terminal */
	.terminal-pane {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		background: #0d0f12;
	}

	.terminal-banner {
		flex-shrink: 0;
		padding: var(--spacing-xs) var(--spacing-md);
		font-size: 0.75rem;
		color: var(--color-text-secondary);
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.terminal-banner.error {
		color: var(--color-error);
	}

	.terminal-banner code {
		font-family: var(--font-mono);
		color: var(--color-text);
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
	.workspace-input {
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

	.item-name {
		font-weight: 500;
		color: var(--color-text);
	}

	.item-task {
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
