import { describe, expect, it } from 'vitest';
import type { SessionInfo, WorkspaceView } from '$lib/types/toren';
import {
	attachCommands,
	ciGlyph,
	ciTone,
	prTooltip,
	splitPrs,
	truncate,
	workspaceFacts,
} from './facts';

function workspace(overrides: Partial<WorkspaceView> = {}): WorkspaceView {
	return {
		name: 'one',
		segment: 'toren',
		path: '/tmp/one',
		decorated: true,
		vcs_tracked: true,
		state: { version: 1 },
		sets: { sessions: [], changes: [], branches: [], prs: [], tasks: [] },
		...overrides,
	};
}

function pane(window: string, status: SessionInfo['status'] = 'idle'): SessionInfo {
	return { window, status, command: 'zsh' };
}

describe('workspaceFacts', () => {
	it('reports nothing to show for an untouched workspace', () => {
		const facts = workspaceFacts(workspace());
		expect(facts.task).toBeNull();
		expect(facts.changes).toEqual([]);
		expect(facts.prs).toEqual([]);
		expect(facts.runs).toBe(0);
		expect(facts.session).toBeNull();
	});

	it('counts recorded runs and names the rmux session', () => {
		const facts = workspaceFacts(
			workspace({
				session: 'toren-toren-one',
				state: {
					version: 1,
					agent: { name: 'claude', sessions: [{ agent: 'claude' }, { agent: 'claude' }] },
				},
			}),
		);
		expect(facts.runs).toBe(2);
		expect(facts.session).toBe('toren-toren-one');
	});

	it('takes the primary task and the collected sets', () => {
		const facts = workspaceFacts(
			workspace({
				sets: {
					sessions: [],
					changes: [{ id: 'abc', summary: 'a change' }],
					branches: ['tor-wxn'],
					prs: [{ branch: 'tor-wxn', id: '#12', url: 'https://x/12', state: 'open', ci: 'pass' }],
					prs_age: '3h',
					tasks: [{ link: 'runes:tor-wxn', source: 'runes', id: 'tor-wxn' }],
				},
			}),
		);
		expect(facts.task?.id).toBe('tor-wxn');
		expect(facts.changes).toHaveLength(1);
		expect(facts.branches).toEqual(['tor-wxn']);
		expect(facts.prsAge).toBe('3h');
	});
});

describe('splitPrs', () => {
	const prs = [1, 2, 3, 4].map((n) => ({
		branch: `b${n}`,
		id: `#${n}`,
		url: `https://x/${n}`,
		state: 'open',
		ci: '',
	}));

	it('keeps everything on the strip when it fits', () => {
		const { shown, overflow } = splitPrs(prs.slice(0, 2));
		expect(shown).toHaveLength(2);
		expect(overflow).toEqual([]);
	});

	it('folds the rest into the overflow', () => {
		const { shown, overflow } = splitPrs(prs);
		expect(shown.map((p) => p.id)).toEqual(['#1', '#2']);
		expect(overflow.map((p) => p.id)).toEqual(['#3', '#4']);
	});
});

describe('ci glyphs', () => {
	it('reads the provider word for the verdict', () => {
		expect(ciTone('passing')).toBe('ok');
		expect(ciTone('SUCCESS')).toBe('ok');
		expect(ciTone('failure')).toBe('bad');
		expect(ciTone('in progress')).toBe('pending');
		expect(ciTone('something else')).toBe('unknown');
	});

	it('says nothing when there is no verdict', () => {
		expect(ciGlyph('')).toBe('');
		expect(ciGlyph('pass')).toBe('✓');
	});
});

describe('prTooltip', () => {
	it('carries branch, state and ci off the chip', () => {
		expect(
			prTooltip({ branch: 'tor-wxn', id: '#12', url: 'https://x/12', state: 'open', ci: 'pass' }),
		).toBe('tor-wxn · open · pass');
	});

	it('skips what the PR does not have', () => {
		expect(
			prTooltip({ branch: 'tor-wxn', id: '#12', url: 'https://x/12', state: 'open', ci: '' }),
		).toBe('tor-wxn · open');
	});
});

describe('attachCommands', () => {
	it('offers a shell of your own, then each live window', () => {
		expect(attachCommands('one', [pane('agent', 'running'), pane('shell')])).toEqual([
			'breq sh one',
			'breq sh one --window agent',
			'breq sh one --window shell',
		]);
	});

	it('leaves out held windows, which have nothing running to watch', () => {
		expect(attachCommands('one', [pane('cmd', 'exited')])).toEqual(['breq sh one']);
	});

	it('quotes names a shell would split', () => {
		expect(attachCommands('my ws', [pane('a b')])).toEqual([
			"breq sh 'my ws'",
			"breq sh 'my ws' --window 'a b'",
		]);
	});

	it('never suggests attaching to the rmux session', () => {
		for (const command of attachCommands('one', [pane('agent')])) {
			expect(command).not.toContain('rmux attach');
		}
	});
});

describe('truncate', () => {
	it('leaves what fits alone', () => {
		expect(truncate('short', 10)).toBe('short');
	});

	it('marks what it cut', () => {
		expect(truncate('a very long title indeed', 10)).toBe('a very lo…');
	});
});
