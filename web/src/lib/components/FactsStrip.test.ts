import { fireEvent, render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import type { AgentSession, WorkspaceView } from '$lib/types/toren';
import FactsStrip from './FactsStrip.svelte';

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

describe('FactsStrip', () => {
	it('says so when a workspace has no task, and shows nothing else', () => {
		render(FactsStrip, { workspace: workspace() });
		expect(screen.getByText('no task')).toBeInTheDocument();
		expect(screen.queryByText(/run/)).not.toBeInTheDocument();
	});

	it('dives into the changes behind the count', async () => {
		render(FactsStrip, {
			workspace: workspace({
				sets: {
					sessions: [],
					changes: [{ id: 'kxmp', summary: 'facts strip' }],
					branches: ['tor-wxn'],
					prs: [],
					tasks: [],
				},
			}),
		});
		expect(screen.queryByText('facts strip')).not.toBeInTheDocument();
		await fireEvent.click(screen.getByLabelText('Changes'));
		expect(screen.getByText('facts strip')).toBeInTheDocument();
		expect(screen.getByText('tor-wxn')).toBeInTheDocument();
	});

	it('opens a PR by its chip and folds the rest into an overflow', () => {
		const prs = [1, 2, 3].map((n) => ({
			branch: `b${n}`,
			id: `#${n}`,
			url: `https://x/${n}`,
			state: 'open',
			ci: 'pass',
		}));
		render(FactsStrip, {
			workspace: workspace({
				sets: { sessions: [], changes: [], branches: [], prs, tasks: [] },
			}),
		});
		expect(screen.getByText('#1').closest('a')).toHaveAttribute('href', 'https://x/1');
		expect(screen.queryByText('#3')).not.toBeInTheDocument();
		expect(screen.getByText('+1')).toBeInTheDocument();
	});

	it('asks for the recorded sessions when its popover opens, and resumes one', async () => {
		let asked = 0;
		let resumed: string | undefined;
		render(FactsStrip, {
			props: {
				workspace: workspace({
					state: { version: 1, agent: { name: 'claude', sessions: [{ agent: 'claude' }] } },
				}),
				sessions: [{ id: 'abc', agent: 'claude', title: 'a run' }],
			},
			events: {
				sessions: () => {
					asked += 1;
				},
				resume: (event: CustomEvent<{ session: AgentSession }>) => {
					resumed = event.detail.session.id;
				},
			},
		});

		await fireEvent.click(screen.getByLabelText('Agent sessions'));
		expect(asked).toBe(1);
		expect(screen.getByText('a run')).toBeInTheDocument();

		await fireEvent.click(screen.getByText('Resume'));
		expect(resumed).toBe('abc');
		expect(screen.queryByText('a run')).not.toBeInTheDocument();
	});

	it('offers the attach commands for the rmux session, never an rmux attach', async () => {
		render(FactsStrip, {
			workspace: workspace({
				session: 'toren-toren-one',
				sets: {
					sessions: [{ window: 'agent', status: 'running', command: 'claude' }],
					changes: [],
					branches: [],
					prs: [],
					tasks: [],
				},
			}),
		});
		await fireEvent.click(screen.getByLabelText('rmux session'));
		expect(screen.getByText('breq sh one')).toBeInTheDocument();
		expect(screen.getByText('breq sh one --window agent')).toBeInTheDocument();
		expect(document.body.textContent).not.toContain('rmux attach');
	});

	it('says a copy did not land rather than pretending it did', async () => {
		const clipboard = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
		Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
		try {
			render(FactsStrip, { workspace: workspace({ session: 'toren-toren-one' }) });
			await fireEvent.click(screen.getByLabelText('rmux session'));
			await fireEvent.click(screen.getByText('breq sh one'));
			expect(screen.getByText('copy failed')).toBeInTheDocument();
		} finally {
			if (clipboard) Object.defineProperty(navigator, 'clipboard', clipboard);
		}
	});
});
