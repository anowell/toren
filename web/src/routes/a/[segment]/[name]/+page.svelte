<script lang="ts">
import { onDestroy } from 'svelte';
import { goto } from '$app/navigation';
import { page } from '$app/stores';
import AgentTerminal from '$lib/components/AgentTerminal.svelte';
import FactsStrip from '$lib/components/FactsStrip.svelte';
import SegmentDropdown from '$lib/components/SegmentDropdown.svelte';
import SessionsModal from '$lib/components/SessionsModal.svelte';
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
import type { HeldAction } from '$lib/terminal/held';
import type {
	AgentInfo,
	AgentSession,
	SessionInfo,
	StartWorkspaceRequest,
	WorkflowVerb,
	WorkspaceView,
} from '$lib/types/toren';

let showMobilePanel = false;

/** Typed structurally so biome doesn't see the import as type-only. */
let terminal: { resync(): void; takeSize(): void } | null = null;
let paneStatus = 'connecting';
let wsError: string | null = null;
// Bumped whenever the pane behind an unchanged window name has been replaced — a resume, another
// agent, or a retry after the terminal gave up. The url is the same either way, so this is the
// only thing that can tell the terminal to leave the pane it is on.
let attachNonce = 0;

// Attaching is normally instant, so saying so is noise; a wait long enough to notice is not.
const ATTACHING_NOTICE_MS = 300;
let attachingSlowly = false;
let attachingTimer: ReturnType<typeof setTimeout> | null = null;

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

/** The agent's own word for what it is doing, which is worth more than its pane's status. */
function paneActivity(s: SessionInfo): string {
	return s.agent_activity ?? '';
}

/** A live agent pane is stopped rather than dismissed: stopping it is what closes it. */
function isLiveAgent(s: SessionInfo): boolean {
	return s.window === 'agent' && s.status !== 'exited';
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
	wsError = null;
	startAttachingTimer();
}

/** Attach to whatever is in the selected window now, forgetting what was there before. */
function reattachPane() {
	attachNonce += 1;
	paneStatus = 'connecting';
	wsError = null;
	startAttachingTimer();
}

function startAttachingTimer() {
	stopAttachingTimer();
	attachingTimer = setTimeout(() => {
		attachingTimer = null;
		attachingSlowly = true;
	}, ATTACHING_NOTICE_MS);
}

function stopAttachingTimer() {
	if (attachingTimer) clearTimeout(attachingTimer);
	attachingTimer = null;
	attachingSlowly = false;
}

$: displayStatus = currentWorkspace ? getWorkspaceDisplayStatus(currentWorkspace) : 'ready';
$: task = currentWorkspace ? primaryTask(currentWorkspace) : null;

function handleTerminalStatus(event: CustomEvent<{ status: string; session?: string }>) {
	paneStatus = event.detail.status;
	if (paneStatus === 'attached') wsError = null;
	// A dropped socket is already on its way back, so it is the same wait as a first attach.
	if (paneStatus === 'disconnected') startAttachingTimer();
	else stopAttachingTimer();
}

/**
 * Who is sizing this pane, and what its grid is.
 *
 * A tab that is not the one sizing it renders the pane's own grid scaled to fit, so the app
 * inside is never asked to lay itself out for two windows at once. Typing here takes the size
 * back; the banner is for taking it without typing.
 */
let sizeOwned = true;
let paneGrid: { cols: number; rows: number } | null = null;

function handleTerminalSizing(event: CustomEvent<{ owned: boolean; cols: number; rows: number }>) {
	sizeOwned = event.detail.owned;
	paneGrid = { cols: event.detail.cols, rows: event.detail.rows };
}

function handleTerminalError(event: CustomEvent<{ message: string }>) {
	wsError = event.detail.message;
	stopAttachingTimer();
}

// The daemon pushes every pane-state change it sees, whether or not this tab has that pane open.
// Without it, a process killed in an unwatched window kept reading "running" in the session list
// until something else happened to refresh the workspace.
let stopFollowingLifecycle: (() => void) | null = null;
$: if ($torenStore.authenticated && !stopFollowingLifecycle) {
	stopFollowingLifecycle = torenStore.followLifecycle($torenStore.shipUrl);
}

onDestroy(() => {
	stopAttachingTimer();
	stopFollowingLifecycle?.();
	stopFollowingLifecycle = null;
	terminal = null;
});

let lifecycleLoading = false;
let lifecycleError: string | null = null;
let shellLoading = false;
let dismissing: string | null = null;

// The agents this daemon can start, so "+ Agent" names them rather than hiding the choice.
let agents: AgentInfo[] = [];
let agentsRequested = false;
let showAgentMenu = false;
let agentMenu: HTMLDivElement;

// The workspace's recorded sessions, loaded when the modal opens rather than with the page.
let showSessions = false;
let recordedSessions: AgentSession[] = [];
let sessionsLoading = false;
let sessionsError: string | null = null;
let resumingId: string | null = null;

// Whether there is anything to resume, which the workspace itself already knows.
$: hasRecordedSessions = (currentWorkspace?.state.agent?.sessions?.length ?? 0) > 0;

// A workflow verb runs a script that rewrites the workspace, so it is confirmed by name first.
let pendingVerb: WorkflowVerb | null = null;
let workflowRunning = false;

$: if ($torenStore.authenticated && !agentsRequested) loadAgents();

async function loadAgents() {
	agentsRequested = true;
	try {
		agents = await torenStore.loadAgents($torenStore.shipUrl);
	} catch {
		// A daemon that cannot list its agents can still start the configured default.
		agents = [];
	}
}

function closeAgentMenu(event: MouseEvent) {
	if (agentMenu && !agentMenu.contains(event.target as Node)) showAgentMenu = false;
}

async function refreshCurrent(): Promise<WorkspaceView | null> {
	if (!currentWorkspace) return null;
	return torenStore.refreshWorkspace(
		$torenStore.shipUrl,
		currentWorkspace.segment,
		currentWorkspace.name,
	);
}

/** Start an agent and attach to it. Returns whether it started. */
async function startAgent(request: StartWorkspaceRequest): Promise<boolean> {
	if (!currentWorkspace || lifecycleLoading) return false;
	lifecycleLoading = true;
	lifecycleError = null;
	try {
		await torenStore.startWorkspace(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
			request,
		);
		await refreshCurrent();
		// The agent window now exists; attach to it. Resuming reuses the window it held, so the
		// terminal is told the pane changed as well — nothing about the url would say so.
		selectWindow('agent');
		reattachPane();
		return true;
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to start';
		return false;
	} finally {
		lifecycleLoading = false;
	}
}

/** "New <agent> agent": a fresh session, with the agent named unless the default will do. */
function handleNewAgent(agent?: string) {
	showAgentMenu = false;
	startAgent(agent ? { agent } : {});
}

function openSessions() {
	showAgentMenu = false;
	showSessions = true;
	loadRecordedSessions();
}

/** The rows behind both the resume modal and the facts strip's runs chip. */
async function loadRecordedSessions() {
	if (!currentWorkspace || sessionsLoading) return;
	sessionsLoading = true;
	sessionsError = null;
	try {
		recordedSessions = await torenStore.loadSessions(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
		);
	} catch (err) {
		sessionsError = err instanceof Error ? err.message : 'Failed to load sessions';
	} finally {
		sessionsLoading = false;
	}
}

/** Resuming is a new pane on an old session, so the pane it came from stays where it was. */
async function handleResumeSession(event: CustomEvent<{ session: AgentSession }>) {
	const session = event.detail.session;
	if (!session.id) return;
	resumingId = session.id;
	const started = await startAgent({ session: session.id, agent: session.agent });
	resumingId = null;
	if (started) {
		showSessions = false;
	} else {
		sessionsError = lifecycleError;
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

/**
 * Run `breq complete` / `breq abort` for this workspace, and go and watch it.
 *
 * The daemon runs it in a held pane rather than reporting an outcome, because these scripts talk:
 * they print for a while, and they stop to ask things. Selecting the window they landed in is the
 * whole point of the button.
 */
async function runWorkflow() {
	if (!currentWorkspace || !pendingVerb || workflowRunning) return;
	workflowRunning = true;
	lifecycleError = null;
	try {
		const target = await torenStore.runWorkflow(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
			pendingVerb,
		);
		pendingVerb = null;
		await refreshCurrent();
		selectWindow(target);
		reattachPane();
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to run';
	} finally {
		workflowRunning = false;
	}
}

/**
 * The three things a held pane's status line offers, from the browser side. The line itself is
 * drawn into the pane's bytes by the mirror, so it reads the same here as in a terminal.
 */
function handleHeld(event: CustomEvent<{ action: HeldAction }>) {
	if (event.detail.action === 'primary') {
		handleHeldPrimary();
	} else if (event.detail.action === 'shell') {
		dropToShell();
	} else {
		dismissWindow(selectedWindow);
	}
}

/**
 * `<ENTER>`: an agent pane resumes the session it was working — the daemon knows which. A shell
 * pane has no command recorded anywhere the browser can reach, so it gets a fresh shell instead
 * of a blind re-run.
 */
function handleHeldPrimary() {
	if (selectedWindow === 'agent') {
		startAgent({ resume: true });
		return;
	}
	handleNewShell();
}

/** `<ESC>`: leave the dead pane for a live one, opening a shell if there is none. */
async function dropToShell() {
	const live = sessions.find((s) => s.window !== selectedWindow && s.status !== 'exited');
	if (live) {
		selectWindow(live.window);
		return;
	}
	await handleNewShell();
}

/**
 * Dismiss a window. Every resume is a new pane and a held one outlives its process on purpose, so
 * these accumulate; getting rid of one is a click from the window list or `<Ctrl-c>` in the pane.
 */
async function dismissWindow(window: string | null) {
	if (!currentWorkspace || !window || dismissing) return;
	dismissing = window;
	lifecycleError = null;
	try {
		await torenStore.closeWorkspaceWindow(
			$torenStore.shipUrl,
			currentWorkspace.segment,
			currentWorkspace.name,
			window,
		);
		const ws = await refreshCurrent();
		if (selectedWindow === window) {
			const left = (ws?.sets.sessions ?? []).filter((s) => s.window !== window);
			selectedWindow = defaultWindowName(left);
		}
	} catch (err) {
		lifecycleError = err instanceof Error ? err.message : 'Failed to dismiss';
	} finally {
		dismissing = null;
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

// Only the daemon's own verdict holds a pane: a socket that gave up says nothing about the process
// behind it, and treating that as held would rebind the keys over a pane that is still running.
$: paneEnded = paneStatus === 'ended';
$: paneUnreachable = paneStatus === 'unreachable';
</script>

<svelte:window on:click={closeAgentMenu} />

{#if showSessions}
	<SessionsModal
		sessions={recordedSessions}
		loading={sessionsLoading}
		error={sessionsError}
		busyId={resumingId}
		on:resume={handleResumeSession}
		on:close={() => (showSessions = false)}
	/>
{/if}

{#if pendingVerb && currentWorkspace}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<div class="modal-overlay" on:click={() => (pendingVerb = null)} role="presentation">
		<div class="modal" on:click|stopPropagation role="dialog" tabindex="-1">
			<h2>{pendingVerb === 'complete' ? 'Complete' : 'Abort'} {currentWorkspace.name}?</h2>
			<p class="modal-body">
				Runs <code>breq {pendingVerb} {currentWorkspace.name}</code> in a held pane, where you can
				read what it says and answer it.
			</p>
			<div class="modal-actions">
				<button class="modal-btn" on:click={() => (pendingVerb = null)}>Cancel</button>
				<button class="modal-btn confirm" on:click={runWorkflow} disabled={workflowRunning}>
					{workflowRunning ? 'Running…' : `Run breq ${pendingVerb}`}
				</button>
			</div>
		</div>
	</div>
{/if}

<div class="workspace-view">
	<!-- App bar -->
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
			<!-- The sidebar is the project scope, and on a phone the app bar is where it lives. -->
			<button class="panel-toggle mobile-only" on:click={toggleMobilePanel} aria-label="View workspaces">
				<svg
					xmlns="http://www.w3.org/2000/svg"
					width="18"
					height="18"
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

	<!-- Ancillary bar: this workspace, and the two verbs that end it -->
	{#if currentWorkspace}
		<div class="workspace-indicator">
			<span
				class="workspace-status-dot"
				class:busy={displayStatus === 'busy'}
				class:ready={displayStatus === 'ready'}
				title={displayStatus === 'busy' ? 'An agent is running' : 'Idle'}
			></span>
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
				{#if lifecycleError}
					<span class="lifecycle-error">{lifecycleError}</span>
				{/if}
				<button
					class="action-btn complete"
					on:click={() => (pendingVerb = 'complete')}
					disabled={workflowRunning}
					title="Run breq complete for this workspace"
				>Complete</button>
				<button
					class="action-btn abort"
					on:click={() => (pendingVerb = 'abort')}
					disabled={workflowRunning}
					title="Run breq abort for this workspace"
				>Abort</button>
			</div>
		</div>

		<!-- Facts strip: what is true of this workspace, one chip at a time -->
		<FactsStrip
			workspace={currentWorkspace}
			sessions={recordedSessions}
			{sessionsLoading}
			{sessionsError}
			busyId={resumingId}
			on:sessions={loadRecordedSessions}
			on:resume={handleResumeSession}
		/>

		<!-- Panes bar: one chip per rmux window, and the two ways to make another -->
		<div class="panes-bar">
			<div class="panes-scroll">
				{#each sessions as s (s.window)}
					<div class="session-chip" class:active={s.window === selectedWindow} class:held={s.status === 'exited'}>
						<!-- The dot is the status; the words behind it are a tooltip, not a bar. -->
						<button class="chip-main" on:click={() => selectWindow(s.window)} title="{s.status} · {s.command}">
							<span class="chip-dot status-{s.status}"></span>
							<span class="chip-name mono">{s.window}</span>
							{#if paneActivity(s)}<span class="chip-activity">{paneActivity(s)}</span>{/if}
							<span class="chip-cmd mono">{s.command}</span>
						</button>
						{#if isLiveAgent(s)}
							<!-- The agent's own pane: stopping it is what closes it, so there is no second verb. -->
							<button
								class="chip-stop"
								on:click={handleStop}
								disabled={lifecycleLoading}
								title="Stop the agent"
								aria-label="Stop the agent"
							>
								<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9" viewBox="0 0 24 24" fill="currentColor">
									<rect x="4" y="4" width="16" height="16" rx="2" />
								</svg>
							</button>
						{:else}
							<!-- Held panes accumulate — every resume leaves one — so dismissal is one click. -->
							<button
								class="chip-close"
								on:click={() => dismissWindow(s.window)}
								disabled={dismissing === s.window}
								title="Dismiss {s.window}"
								aria-label="Dismiss {s.window}"
							>×</button>
						{/if}
					</div>
				{/each}
			</div>
			<div class="pane-actions">
				<button
					class="pane-btn"
					on:click={handleNewShell}
					disabled={shellLoading}
					title="Open a new shell window"
				>{shellLoading ? 'Opening…' : '+ Shell'}</button>
				<div class="agent-menu" bind:this={agentMenu}>
					<button
						class="pane-btn"
						on:click={() => (showAgentMenu = !showAgentMenu)}
						disabled={lifecycleLoading}
						aria-expanded={showAgentMenu}
						title="Start an agent in this workspace"
					>+ Agent ▾</button>
					{#if showAgentMenu}
						<div class="agent-menu-list">
							{#each agents as agent (agent.name)}
								<button class="agent-menu-item" on:click={() => handleNewAgent(agent.name)}>
									New {agent.name} agent
								</button>
							{:else}
								<!-- No agent this daemon can start is named, so the default is the only choice. -->
								<button class="agent-menu-item" on:click={() => handleNewAgent()}>New agent</button>
							{/each}
							{#if hasRecordedSessions}
								<div class="agent-menu-divider"></div>
								<button class="agent-menu-item" on:click={openSessions}>Resume previous session…</button>
							{/if}
						</div>
					{/if}
				</div>
			</div>
		</div>
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
			{#if paneUnreachable}
				<!-- The pane is very likely fine; it is the daemon between here and it that is not. -->
				<div class="terminal-banner error">
					Lost contact with this pane.
					<button class="banner-btn" on:click={reattachPane}>Retry</button>
				</div>
			{:else if wsError}
				<div class="terminal-banner error">
					{wsError}
					<button class="banner-btn" on:click={() => terminal?.resync()}>Resync</button>
				</div>
			{:else if paneEnded}
				<!-- The pane draws its own exit line; these are the same three keys, for a mouse. -->
				<div class="terminal-banner">
					This pane has exited and is being held.
					<button class="banner-btn" on:click={handleHeldPrimary}>
						{selectedWindow === 'agent' ? 'Resume session' : 'New shell'}
					</button>
					<button class="banner-btn" on:click={dropToShell}>Drop to shell</button>
					<button class="banner-btn" on:click={() => dismissWindow(selectedWindow)} disabled={dismissing !== null}>Dismiss</button>
				</div>
			{:else if paneStatus === 'degraded'}
				<!--
					The daemon has lost its grip on a pane that is still running. What is on screen is
					the last thing it saw, not the end of anything, and it starts moving again by
					itself — so this says "stale", never "exited".
				-->
				<div class="terminal-banner">
					This pane is still running; the view of it is stale.
					<button class="banner-btn" on:click={() => terminal?.resync()}>Resync</button>
				</div>
			{:else if !sizeOwned && paneGrid}
				<!--
					One PTY, one geometry. Another viewer — a terminal, or another tab — is the one it
					is laid out for, so this one shows that layout scaled rather than reflowing a
					screen its app never composed.
				-->
				<div class="terminal-banner">
					Sized {paneGrid.cols}×{paneGrid.rows} by another viewer.
					<button class="banner-btn" on:click={() => terminal?.takeSize()}>Resize to this window</button>
				</div>
			{:else if attachingSlowly}
				<!-- Attaching is normally too quick to see; a wait long enough to notice is not. -->
				<div class="terminal-banner">Attaching…</div>
			{/if}
			<AgentTerminal
				bind:this={terminal}
				url={terminalUrl}
				{attachNonce}
				held={paneEnded}
				on:status={handleTerminalStatus}
				on:sizing={handleTerminalSizing}
				on:error={handleTerminalError}
				on:held={handleHeld}
			/>
		{/if}
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
		min-height: 0;
		background: var(--color-bg);
	}

	/* App bar */
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

	/* Ancillary bar */
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
		flex-shrink: 0;
	}

	.action-btn {
		padding: 2px 10px;
		border-radius: var(--radius-sm);
		font-size: 0.7rem;
		font-weight: 600;
		text-transform: uppercase;
		cursor: pointer;
		border: 1px solid transparent;
		background: none;
	}

	.action-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.action-btn.complete {
		color: var(--color-success);
		border-color: var(--color-success);
	}

	.action-btn.complete:hover:not(:disabled) {
		background: var(--color-success);
		color: var(--color-bg);
	}

	.action-btn.abort {
		color: var(--color-error);
		border-color: var(--color-error);
	}

	.action-btn.abort:hover:not(:disabled) {
		background: var(--color-error);
		color: white;
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

	/* Confirm dialog */
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
		max-width: 420px;
		width: 100%;
	}

	.modal h2 {
		margin: 0 0 var(--spacing-sm) 0;
		color: var(--color-text);
		font-size: 1.1rem;
	}

	.modal-body {
		margin: 0 0 var(--spacing-lg) 0;
		color: var(--color-text-secondary);
		font-size: 0.85rem;
	}

	.modal-body code {
		font-family: var(--font-mono);
		color: var(--color-text);
	}

	.modal-actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--spacing-sm);
	}

	.modal-btn {
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-text);
		font-size: 0.85rem;
		cursor: pointer;
	}

	.modal-btn:hover:not(:disabled) {
		border-color: var(--color-primary);
	}

	.modal-btn.confirm {
		background: var(--color-primary);
		border-color: var(--color-primary);
		color: white;
	}

	.modal-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.mono {
		font-family: var(--font-mono);
	}

	.badge {
		font-size: 0.65rem;
		padding: 1px 6px;
		border-radius: var(--radius-sm);
		background: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
	}

	/* Panes bar (rmux windows) */
	.panes-bar {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-secondary);
		border-bottom: 1px solid var(--color-border);
		flex-shrink: 0;
	}

	/* Only the chips scroll: a scrolling bar would clip the agent menu and carry off the buttons
	   that open it. */
	.panes-scroll {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		flex: 1;
		min-width: 0;
		overflow-x: auto;
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

	.session-chip:hover {
		border-color: var(--color-primary);
		color: var(--color-text);
	}

	.session-chip.held {
		border-style: dashed;
	}

	.chip-main {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		background: none;
		border: none;
		padding: 0;
		color: inherit;
		font-size: inherit;
		cursor: pointer;
	}

	.chip-close,
	.chip-stop {
		display: flex;
		align-items: center;
		background: none;
		border: none;
		padding: 0 2px;
		color: var(--color-text-secondary);
		font-size: 0.9rem;
		line-height: 1;
		cursor: pointer;
	}

	.chip-close:hover:not(:disabled),
	.chip-stop:hover:not(:disabled) {
		color: var(--color-error);
	}

	.chip-close:disabled,
	.chip-stop:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.session-chip.active {
		border-color: var(--color-primary);
		background: var(--color-bg-tertiary);
		color: var(--color-text);
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

	.chip-activity {
		color: var(--color-text-secondary);
		opacity: 0.7;
		font-style: italic;
	}

	.chip-cmd {
		color: var(--color-text-secondary);
		max-width: 14ch;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.pane-actions {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding-left: var(--spacing-sm);
		flex-shrink: 0;
	}

	.pane-btn {
		padding: 3px 10px;
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		color: var(--color-primary);
		font-size: 0.75rem;
		font-weight: 600;
		white-space: nowrap;
		cursor: pointer;
	}

	.pane-btn:hover:not(:disabled) {
		border-color: var(--color-primary);
	}

	.pane-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.agent-menu {
		position: relative;
	}

	.agent-menu-list {
		position: absolute;
		top: calc(100% + 4px);
		right: 0;
		min-width: 200px;
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
		z-index: 100;
		overflow: hidden;
	}

	.agent-menu-item {
		display: block;
		width: 100%;
		padding: var(--spacing-sm) var(--spacing-md);
		background: none;
		border: none;
		color: var(--color-text);
		font-size: 0.85rem;
		text-align: left;
		white-space: nowrap;
		cursor: pointer;
	}

	.agent-menu-item:hover {
		background: var(--color-bg-tertiary);
	}

	.agent-menu-divider {
		height: 1px;
		background: var(--color-border);
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

	.banner-btn {
		margin-left: var(--spacing-xs);
		padding: 1px 8px;
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		color: var(--color-text);
		font-size: 0.7rem;
		cursor: pointer;
	}

	.banner-btn:hover:not(:disabled) {
		border-color: var(--color-primary);
	}

	.banner-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
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

	.panel-toggle {
		width: 32px;
		height: 32px;
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
		top: -6px;
		right: -6px;
		min-width: 16px;
		height: 16px;
		padding: 0 4px;
		background: var(--color-warning);
		color: var(--color-bg);
		font-size: 0.65rem;
		font-weight: 700;
		border-radius: 8px;
		display: flex;
		align-items: center;
		justify-content: center;
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
		background: var(--color-text-secondary);
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
