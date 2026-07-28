import { describe, expect, it } from 'vitest';
import { heldAction } from './held';

describe('heldAction', () => {
	it('maps the three keys the status line offers', () => {
		expect(heldAction('\r')).toBe('primary');
		expect(heldAction('\x1b')).toBe('shell');
		expect(heldAction('\x03')).toBe('close');
	});

	it('ignores an escape that opens a sequence', () => {
		expect(heldAction('\x1b[A')).toBeNull();
		expect(heldAction('\x1b[<0;1;1M')).toBeNull();
	});

	it('ignores ordinary typing', () => {
		expect(heldAction('a')).toBeNull();
		expect(heldAction('')).toBeNull();
	});
});
