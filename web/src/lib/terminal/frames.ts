/**
 * The pane bridge's binary framing.
 *
 * Every frame the daemon sends opens with a big-endian u32 epoch. The daemon re-seeds a mirror by
 * painting the pane's whole screen, so bytes from before a paint describe a screen that no longer
 * exists — applying them over the paint is what used to leave a terminal corrupted until the page
 * was reloaded. Comparing epochs is what makes a resync a resync rather than a race.
 */

/** Bytes of epoch that precede a frame's payload. */
export const FRAME_HEADER_BYTES = 4;

export interface PaneFrame {
	epoch: number;
	bytes: Uint8Array;
}

/** Split a frame into the screen generation it belongs to and the bytes to apply. */
export function decodeFrame(data: ArrayBuffer): PaneFrame | null {
	if (data.byteLength < FRAME_HEADER_BYTES) return null;
	return {
		epoch: new DataView(data).getUint32(0),
		bytes: new Uint8Array(data, FRAME_HEADER_BYTES),
	};
}

/**
 * What to do with a frame:
 * - `discard` — it belongs to a screen we have already been moved off.
 * - `repaint` — it opens a new screen, so clear before applying it.
 * - `apply` — more of the screen we are on.
 */
export type FrameAction = 'discard' | 'repaint' | 'apply';

/** Tracks which screen generation a terminal is showing, and sorts frames against it. */
export class EpochFilter {
	private epoch = -1;

	accept(frame: PaneFrame): FrameAction {
		if (frame.epoch < this.epoch) return 'discard';
		const opened = frame.epoch > this.epoch;
		this.epoch = frame.epoch;
		return opened ? 'repaint' : 'apply';
	}

	/** Start over — a reconnect lands on whatever the pane looks like now. */
	reset(): void {
		this.epoch = -1;
	}

	get current(): number {
		return this.epoch;
	}
}
