import { describe, expect, it } from 'vitest';
import type { AncillaryStatus } from '$lib/types/toren';
import { getAncillaryDisplayStatus, getBeadDisplayStatus, stripBeadPrefix } from './toren';

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
	it('maps pending to open', () => {
		expect(getBeadDisplayStatus('pending')).toBe('open');
	});

	it('maps active to in_progress', () => {
		expect(getBeadDisplayStatus('active')).toBe('in_progress');
	});

	it('maps completed to closed', () => {
		expect(getBeadDisplayStatus('completed')).toBe('closed');
	});

	it('maps aborted to closed', () => {
		expect(getBeadDisplayStatus('aborted')).toBe('closed');
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
