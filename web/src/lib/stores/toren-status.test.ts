import { describe, expect, it } from 'vitest';
import type { SessionInfo, TaskView, WorkspaceView } from '$lib/types/toren';
import {
	defaultWindowName,
	getTaskDisplayStatus,
	getWorkspaceDisplayStatus,
	stripTaskPrefix,
} from './toren';

function makeWorkspace(sessions: SessionInfo[] = []): WorkspaceView {
	return {
		name: 'one',
		segment: 'test',
		uid: 'ab12',
		path: '/tmp/test',
		title: null,
		base: null,
		parent: null,
		decorated: true,
		vcs_tracked: true,
		annotations: {},
		sets: {
			sessions,
			changes: [],
			branches: [],
			prs: [],
			tasks: [],
		},
	};
}

function makeTask(overrides: Partial<TaskView> = {}): TaskView {
	return {
		link: 'beads:breq-test',
		source: 'beads',
		id: 'breq-test',
		...overrides,
	};
}

describe('getWorkspaceDisplayStatus', () => {
	it('is busy when any session is running', () => {
		const ws = makeWorkspace([
			{ window: 'agent', status: 'idle', command: 'zsh' },
			{ window: 'run', status: 'running', command: 'claude' },
		]);
		expect(getWorkspaceDisplayStatus(ws)).toBe('busy');
	});

	it('is ready when no session is running', () => {
		const ws = makeWorkspace([{ window: 'agent', status: 'idle', command: 'zsh' }]);
		expect(getWorkspaceDisplayStatus(ws)).toBe('ready');
	});

	it('is ready when there are no sessions', () => {
		expect(getWorkspaceDisplayStatus(makeWorkspace())).toBe('ready');
	});
});

describe('defaultWindowName', () => {
	const shell: SessionInfo = { window: 'shell', status: 'idle', command: 'zsh' };
	const shell2: SessionInfo = { window: 'shell-2', status: 'idle', command: 'zsh' };
	const agent: SessionInfo = { window: 'agent', status: 'running', command: 'claude' };

	it('prefers the agent window when present', () => {
		expect(defaultWindowName([shell, agent, shell2])).toBe('agent');
	});

	it('falls back to the birth shell when there is no agent', () => {
		expect(defaultWindowName([shell2, shell])).toBe('shell');
	});

	it('uses the first window when neither agent nor shell is named', () => {
		expect(defaultWindowName([shell2])).toBe('shell-2');
	});

	it('returns null when there are no windows', () => {
		expect(defaultWindowName([])).toBeNull();
	});
});

describe('getTaskDisplayStatus', () => {
	it('maps open-ish statuses to open', () => {
		expect(getTaskDisplayStatus(makeTask({ status: 'open' }))).toBe('open');
		expect(getTaskDisplayStatus(makeTask({ status: 'todo' }))).toBe('open');
	});

	it('maps in-progress statuses to in_progress', () => {
		expect(getTaskDisplayStatus(makeTask({ status: 'in_progress' }))).toBe('in_progress');
		expect(getTaskDisplayStatus(makeTask({ status: 'active' }))).toBe('in_progress');
	});

	it('maps closed/done statuses to closed', () => {
		expect(getTaskDisplayStatus(makeTask({ status: 'closed' }))).toBe('closed');
		expect(getTaskDisplayStatus(makeTask({ status: 'done' }))).toBe('closed');
	});

	it('defaults to in_progress when status is absent', () => {
		expect(getTaskDisplayStatus(makeTask())).toBe('in_progress');
	});
});

describe('stripTaskPrefix', () => {
	it('strips prefix before first hyphen', () => {
		expect(stripTaskPrefix('toren-a3f')).toBe('a3f');
	});

	it('strips before a colon separator too', () => {
		expect(stripTaskPrefix('beads:abc')).toBe('abc');
	});

	it('strips only up to first separator', () => {
		expect(stripTaskPrefix('breq-abc-def')).toBe('abc-def');
	});

	it('returns full string when no separator', () => {
		expect(stripTaskPrefix('a3f')).toBe('a3f');
	});

	it('handles empty string', () => {
		expect(stripTaskPrefix('')).toBe('');
	});
});
