import { describe, expect, it } from 'vitest';
import { decodeFrame, EpochFilter, type PaneFrame } from './frames';

function framed(epoch: number, payload: string): ArrayBuffer {
	const bytes = new TextEncoder().encode(payload);
	const buffer = new ArrayBuffer(4 + bytes.length);
	new DataView(buffer).setUint32(0, epoch);
	new Uint8Array(buffer).set(bytes, 4);
	return buffer;
}

function frame(epoch: number): PaneFrame {
	return { epoch, bytes: new Uint8Array() };
}

describe('decodeFrame', () => {
	it('reads the epoch off the front and leaves the rest alone', () => {
		const decoded = decodeFrame(framed(258, 'hello'));
		expect(decoded?.epoch).toBe(258);
		expect(new TextDecoder().decode(decoded?.bytes)).toBe('hello');
	});

	it('carries an epoch with no payload', () => {
		const decoded = decodeFrame(framed(7, ''));
		expect(decoded?.epoch).toBe(7);
		expect(decoded?.bytes.length).toBe(0);
	});

	it('rejects anything too short to be addressed', () => {
		expect(decodeFrame(new ArrayBuffer(3))).toBeNull();
	});
});

describe('EpochFilter', () => {
	it('repaints on the first frame and streams the rest of that screen', () => {
		const filter = new EpochFilter();
		expect(filter.accept(frame(4))).toBe('repaint');
		expect(filter.accept(frame(4))).toBe('apply');
		expect(filter.current).toBe(4);
	});

	it('discards frames from a screen it has been moved off', () => {
		const filter = new EpochFilter();
		filter.accept(frame(2));
		expect(filter.accept(frame(3))).toBe('repaint');
		expect(filter.accept(frame(2))).toBe('discard');
		expect(filter.accept(frame(3))).toBe('apply');
	});

	it('takes the pane as it finds it after a reconnect', () => {
		const filter = new EpochFilter();
		filter.accept(frame(9));
		filter.reset();
		expect(filter.accept(frame(0))).toBe('repaint');
	});
});
