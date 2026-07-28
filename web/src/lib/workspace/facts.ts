/**
 * What the facts strip knows about a workspace.
 *
 * Every chip is a fact with a count or an id, and an empty fact is not a chip: a zero says nothing
 * worth a line of the terminal's height. The task is the one exception — having no task is itself
 * information — so it is reported as null rather than dropped.
 */

import { primaryTask } from '$lib/stores/toren';
import type { CommitInfo, PrInfo, SessionInfo, TaskView, WorkspaceView } from '$lib/types/toren';

/** How many PR chips the strip shows before the rest fold into one "+N" chip. */
export const PR_CHIP_LIMIT = 2;

export interface Facts {
	/** The workspace's primary task, or null — either way there is a chip. */
	task: TaskView | null;
	changes: CommitInfo[];
	branches: string[];
	prs: PrInfo[];
	/** How stale the PR read is, e.g. "3h". */
	prsAge: string | null;
	/** Recorded agent sessions: runs that happened here, live or not. */
	runs: number;
	/** The rmux session this workspace's panes live in, as the daemon names it. */
	session: string | null;
}

export function workspaceFacts(ws: WorkspaceView): Facts {
	return {
		task: primaryTask(ws),
		changes: ws.sets.changes,
		branches: ws.sets.branches,
		prs: ws.sets.prs,
		prsAge: ws.sets.prs_age ?? null,
		runs: ws.state.agent?.sessions?.length ?? 0,
		session: ws.session ?? null,
	};
}

/** The PRs that get a chip each, and the ones behind the "+N" that follows them. */
export function splitPrs(
	prs: PrInfo[],
	limit = PR_CHIP_LIMIT,
): { shown: PrInfo[]; overflow: PrInfo[] } {
	return { shown: prs.slice(0, limit), overflow: prs.slice(limit) };
}

/** How a CI verdict reads at a glance, when the word itself belongs in a tooltip. */
export type CiTone = 'ok' | 'bad' | 'pending' | 'unknown';

const CI_GLYPHS: Record<CiTone, string> = {
	ok: '✓',
	bad: '✕',
	pending: '◍',
	unknown: '·',
};

export function ciTone(ci: string): CiTone {
	const state = ci.toLowerCase();
	if (state.includes('pass') || state.includes('success') || state.includes('green')) return 'ok';
	if (state.includes('fail') || state.includes('error') || state.includes('red')) return 'bad';
	if (state.includes('pend') || state.includes('run') || state.includes('progress')) {
		return 'pending';
	}
	return 'unknown';
}

/** The glyph for a CI verdict; nothing at all when the PR has no verdict yet. */
export function ciGlyph(ci: string): string {
	return ci ? CI_GLYPHS[ciTone(ci)] : '';
}

/** Everything about a PR that does not fit on its chip. */
export function prTooltip(pr: PrInfo): string {
	return [pr.branch, pr.state, pr.ci].filter(Boolean).join(' · ');
}

const SHELL_SAFE = /^[A-Za-z0-9._:@%+=,/-]+$/;

/** Quote a name that a copied command line would otherwise split on. */
function shellArg(value: string): string {
	if (value === '') return "''";
	return SHELL_SAFE.test(value) ? value : `'${value.replaceAll("'", "'\\''")}'`;
}

/**
 * The commands that put a terminal on this workspace's panes: a shell of one's own first, then
 * each live window by name.
 *
 * Never `rmux attach` — that drops a second client onto a session breq is already mirroring, and
 * the two fight over its size.
 */
export function attachCommands(name: string, sessions: SessionInfo[]): string[] {
	const ws = shellArg(name);
	const live = sessions.filter((s) => s.status !== 'exited');
	return [`breq sh ${ws}`, ...live.map((s) => `breq sh ${ws} --window ${shellArg(s.window)}`)];
}

/** Shorten for a chip: the chip is the glance, and the whole of it is a popover away. */
export function truncate(text: string, max: number): string {
	return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}
