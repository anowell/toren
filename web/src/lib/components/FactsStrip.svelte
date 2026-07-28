<script lang="ts">
import { createEventDispatcher, onDestroy } from 'svelte';
import Popover from '$lib/components/Popover.svelte';
import SessionList from '$lib/components/SessionList.svelte';
import TaskStatusIcon from '$lib/components/TaskStatusIcon.svelte';
import { getTaskDisplayStatus, stripTaskPrefix } from '$lib/stores/toren';
import type { AgentSession, WorkspaceView } from '$lib/types/toren';
import { copyText } from '$lib/workspace/clipboard';
import {
	attachCommands,
	ciGlyph,
	ciTone,
	prTooltip,
	splitPrs,
	truncate,
	workspaceFacts,
} from '$lib/workspace/facts';

// biome-ignore-start lint/style/useConst: svelte props are reassigned by the parent
export let workspace: WorkspaceView;
/** The recorded sessions the page has loaded, shared with the resume modal. */
export let sessions: AgentSession[] = [];
export let sessionsLoading = false;
export let sessionsError: string | null = null;
export let busyId: string | null = null;
// biome-ignore-end lint/style/useConst: svelte props are reassigned by the parent

const dispatch = createEventDispatcher<{
	/** The runs popover was opened, so its rows are worth fetching now. */
	sessions: null;
	resume: { session: AgentSession };
}>();

$: facts = workspaceFacts(workspace);
$: task = facts.task;
$: prs = splitPrs(facts.prs);
$: commands = attachCommands(workspace.name, workspace.sets.sessions);

// What the last copy did, so the row it came from says whether it landed — a browser with no
// clipboard to write to must not leave the row reading "copy" as if nothing had happened.
let lastCopy: { command: string; ok: boolean } | null = null;
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

async function copy(command: string) {
	lastCopy = { command, ok: await copyText(command) };
	if (copiedTimer) clearTimeout(copiedTimer);
	copiedTimer = setTimeout(() => {
		copiedTimer = null;
		lastCopy = null;
	}, 1500);
}

function handleResume(event: CustomEvent<{ session: AgentSession }>, close: () => void) {
	close();
	dispatch('resume', event.detail);
}

onDestroy(() => {
	if (copiedTimer) clearTimeout(copiedTimer);
});
</script>

<div class="facts-strip">
	{#if task}
		<Popover label="Task {stripTaskPrefix(task.id)}" title={task.title ?? task.link}>
			<svelte:fragment slot="chip">
				<TaskStatusIcon status={getTaskDisplayStatus(task)} />
				<span class="mono">{stripTaskPrefix(task.id)}</span>
				{#if task.title}<span class="chip-text">{truncate(task.title, 32)}</span>{/if}
			</svelte:fragment>
			<div class="pop-rows">
				<div class="pop-title">{task.title ?? stripTaskPrefix(task.id)}</div>
				<div class="pop-row">
					<span class="pop-key">Status</span>
					<span>{task.status ?? 'unknown'}</span>
				</div>
				<div class="pop-row">
					<span class="pop-key">Assignee</span>
					<span>{task.assignee ?? 'unassigned'}</span>
				</div>
				{#if task.age}
					<div class="pop-row dim">read {task.age} ago</div>
				{/if}
				{#if task.error}
					<div class="pop-row error">{task.error}</div>
				{/if}
				{#if task.url}
					<a class="pop-link" href={task.url} target="_blank" rel="noreferrer">Open in tracker</a>
				{/if}
			</div>
		</Popover>
	{:else}
		<span class="fact-chip absent" title="No task is linked to this workspace">no task</span>
	{/if}

	{#if facts.changes.length > 0}
		<Popover
			label="Changes"
			title="{facts.changes.length} change{facts.changes.length === 1 ? '' : 's'} here"
		>
			<svelte:fragment slot="chip">
				<span class="glyph">⑂</span>
				<span>{facts.changes.length}</span>
			</svelte:fragment>
			<div class="pop-rows">
				{#if facts.branches.length > 0}
					<div class="pop-badges">
						{#each facts.branches as branch (branch)}
							<span class="badge mono">{branch}</span>
						{/each}
					</div>
				{/if}
				{#each facts.changes as change (change.id)}
					<div class="pop-row">
						<span class="mono dim">{change.id}</span>
						<span class="pop-summary">{change.summary}</span>
					</div>
				{/each}
			</div>
		</Popover>
	{/if}

	<!-- One PR has one obvious destination, so its chip is the link rather than a popover. -->
	{#each prs.shown as pr (pr.id)}
		<a class="fact-chip" href={pr.url} target="_blank" rel="noreferrer" title={prTooltip(pr)}>
			<span class="glyph">⇈</span>
			<span class="mono">{pr.id}</span>
			<span>{pr.state}</span>
			{#if pr.ci}<span class="ci ci-{ciTone(pr.ci)}">{ciGlyph(pr.ci)}</span>{/if}
		</a>
	{/each}
	{#if prs.overflow.length > 0}
		<Popover label="More pull requests" title="{prs.overflow.length} more PRs">
			<svelte:fragment slot="chip">+{prs.overflow.length}</svelte:fragment>
			<div class="pop-rows">
				{#if facts.prsAge}
					<div class="pop-row dim">read {facts.prsAge} ago</div>
				{/if}
				{#each prs.overflow as pr (pr.id)}
					<a class="pop-row link" href={pr.url} target="_blank" rel="noreferrer">
						<span class="mono">{pr.id}</span>
						<span class="mono dim">{pr.branch}</span>
						<span class="badge">{pr.state}</span>
						{#if pr.ci}<span class="badge">{pr.ci}</span>{/if}
					</a>
				{/each}
			</div>
		</Popover>
	{/if}

	{#if facts.runs > 0}
		<Popover
			label="Agent sessions"
			title="{facts.runs} agent session{facts.runs === 1 ? '' : 's'} recorded here"
			on:open={() => dispatch('sessions', null)}
		>
			<svelte:fragment slot="chip">
				<span class="glyph">▣</span>
				<span>{facts.runs} run{facts.runs === 1 ? '' : 's'}</span>
			</svelte:fragment>
			<svelte:fragment let:close>
				<SessionList
					{sessions}
					loading={sessionsLoading}
					error={sessionsError}
					{busyId}
					on:resume={(event) => handleResume(event, close)}
				/>
			</svelte:fragment>
		</Popover>
	{/if}

	{#if facts.session}
		<Popover label="rmux session" title={facts.session}>
			<svelte:fragment slot="chip">
				<span class="glyph mono">rmux</span>
				<span class="mono">{truncate(facts.session, 20)}</span>
			</svelte:fragment>
			<div class="pop-rows">
				<div class="pop-title mono">{facts.session}</div>
				<div class="pop-key">Attach a terminal to it</div>
				{#each commands as command (command)}
					<button class="pop-copy" on:click={() => copy(command)} title="Copy to clipboard">
						<code class="mono">{command}</code>
						{#if lastCopy?.command === command}
							<span class:dim={lastCopy.ok} class:error={!lastCopy.ok}>
								{lastCopy.ok ? 'copied' : 'copy failed'}
							</span>
						{:else}
							<span class="dim">copy</span>
						{/if}
					</button>
				{/each}
			</div>
		</Popover>
	{/if}
</div>

<style>
	/* Facts bar: one line of chips, scrolled sideways when a phone runs out of room. */
	.facts-strip {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border-bottom: 1px solid var(--color-border);
		overflow-x: auto;
		flex-shrink: 0;
	}

	.glyph {
		color: var(--color-text-secondary);
	}

	.chip-text {
		max-width: 32ch;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.ci-ok {
		color: var(--color-success);
	}

	.ci-bad {
		color: var(--color-error);
	}

	.ci-pending {
		color: var(--color-warning);
	}

	.pop-rows {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.pop-title {
		color: var(--color-text);
		font-weight: 600;
		overflow-wrap: anywhere;
	}

	.pop-row {
		display: flex;
		align-items: baseline;
		gap: var(--spacing-xs);
		color: var(--color-text);
		overflow: hidden;
	}

	.pop-row.link {
		text-decoration: none;
	}

	.pop-row.link:hover {
		text-decoration: underline;
	}

	.error {
		color: var(--color-error);
	}

	.pop-key {
		color: var(--color-text-secondary);
		min-width: 6ch;
	}

	.pop-summary {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pop-badges {
		display: flex;
		flex-wrap: wrap;
		gap: var(--spacing-xs);
		margin-bottom: var(--spacing-xs);
	}

	.pop-link {
		margin-top: var(--spacing-xs);
		color: var(--color-primary);
		text-decoration: none;
	}

	.pop-link:hover {
		text-decoration: underline;
	}

	.pop-copy {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--spacing-sm);
		width: 100%;
		padding: 2px 6px;
		background: var(--color-bg);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		color: var(--color-text);
		font-size: 0.75rem;
		text-align: left;
	}

	.pop-copy:hover {
		border-color: var(--color-primary);
	}

	.badge {
		font-size: 0.65rem;
		padding: 1px 6px;
		border-radius: var(--radius-sm);
		background: var(--color-bg-tertiary);
		color: var(--color-text-secondary);
		flex-shrink: 0;
	}

	.mono {
		font-family: var(--font-mono);
	}

	.dim {
		color: var(--color-text-secondary);
	}
</style>
