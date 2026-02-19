import { describe, expect, it } from 'vitest';
import type { AncillaryStatus, Assignment } from '$lib/types/toren';
import { getAncillaryDisplayStatus, getBeadDisplayStatus, stripBeadPrefix } from './toren';

function makeAssignment(overrides: Partial<Assignment> = {}): Assignment {
	return {
		id: 'test',
		ancillary_id: 'Test One',
		bead_id: 'breq-test',
		segment: 'test',
		workspace_path: '/tmp/test',
		source: { type: 'Prompt' },
		status: 'active',
		created_at: '',
		updated_at: '',
		...overrides,
	};
}

describe('getAncillaryDisplayStatus', () => {
	it('maps busy statuses correctly', () => {
		const busyStatuses: AncillaryStatus[] = ['starting', 'working', 'executing'];
		for (const status of busyStatuses) {
			expect(getAncillaryDisplayStatus(status)).toBe('busy');
		}
	});

	it('maps ready statuses correctly', () => {
		const readyStatuses: AncillaryStatus[] = [
			'idle',
			'awaiting_input',
			'completed',
			'failed',
			'connected',
			'disconnected',
		];
		for (const status of readyStatuses) {
			expect(getAncillaryDisplayStatus(status)).toBe('ready');
		}
	});
});

describe('getBeadDisplayStatus', () => {
	it('maps open bead_status to open', () => {
		expect(getBeadDisplayStatus(makeAssignment({ bead_status: 'open' }))).toBe('open');
	});

	it('maps in_progress bead_status to in_progress', () => {
		expect(getBeadDisplayStatus(makeAssignment({ bead_status: 'in_progress' }))).toBe(
			'in_progress',
		);
	});

	it('maps closed bead_status to closed', () => {
		expect(getBeadDisplayStatus(makeAssignment({ bead_status: 'closed' }))).toBe('closed');
	});

	it('defaults to in_progress when bead_status is undefined', () => {
		expect(getBeadDisplayStatus(makeAssignment())).toBe('in_progress');
	});
});

describe('stripBeadPrefix', () => {
	it('strips prefix before first hyphen', () => {
		expect(stripBeadPrefix('toren-a3f')).toBe('a3f');
	});

	it('strips only up to first hyphen', () => {
		expect(stripBeadPrefix('breq-abc-def')).toBe('abc-def');
	});

	it('returns full string when no hyphen', () => {
		expect(stripBeadPrefix('a3f')).toBe('a3f');
	});

	it('handles empty string', () => {
		expect(stripBeadPrefix('')).toBe('');
	});
});
