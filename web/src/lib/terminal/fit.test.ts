import { describe, expect, it } from 'vitest';
import { type Box, contentBox, MIN_COLS, MIN_ROWS, proposeGeometry, scaleToFit } from './fit';

const CELL = { width: 8, height: 17 };

describe('proposeGeometry', () => {
	it('fits whole cells and leaves the remainder as slack', () => {
		const geometry = proposeGeometry({ width: 814, height: 349 }, CELL, 14);
		expect(geometry).toEqual({ cols: 100, rows: 20 });
	});

	it('never proposes a grid taller than the box it was given', () => {
		for (let height = 1; height <= 400; height++) {
			for (const cellHeight of [15, 16.5, 17.328, 21]) {
				const geometry = proposeGeometry({ width: 400, height }, { width: 8, height: cellHeight });
				if (!geometry) continue;
				const grid = geometry.rows * cellHeight;
				// One row is the floor xterm imposes, so a box shorter than a cell is the one case
				// where the grid is taller than the box — there is no smaller terminal to ask for.
				if (geometry.rows > MIN_ROWS) expect(grid).toBeLessThanOrEqual(height);
			}
		}
	});

	it('keeps the scrollbar strip clear of text', () => {
		expect(proposeGeometry({ width: 128, height: 100 }, CELL, 0)?.cols).toBe(16);
		expect(proposeGeometry({ width: 128, height: 100 }, CELL, 14)?.cols).toBe(14);
	});

	it('refuses to divide by a cell that has not been measured', () => {
		expect(proposeGeometry({ width: 800, height: 400 }, { width: 0, height: 0 })).toBeNull();
		expect(
			proposeGeometry({ width: 800, height: 400 }, { width: 8, height: Number.NaN }),
		).toBeNull();
	});

	it('refuses a box with no room in it', () => {
		expect(proposeGeometry({ width: 800, height: 0 }, CELL)).toBeNull();
		expect(proposeGeometry({ width: 10, height: 400 }, CELL, 14)).toBeNull();
	});

	it('holds the floor for a box barely bigger than nothing', () => {
		expect(proposeGeometry({ width: 20, height: 4 }, CELL, 0)).toEqual({
			cols: MIN_COLS,
			rows: MIN_ROWS,
		});
	});
});

/**
 * An element measuring `box`, inset by `padding` and `border` on every side.
 *
 * Connected and written as longhands because that is all happy-dom resolves a computed style from.
 */
function inset(box: Box, padding: number, border = 0): HTMLElement {
	const el = document.createElement('div');
	for (const side of ['top', 'right', 'bottom', 'left']) {
		el.style.setProperty(`padding-${side}`, `${padding}px`);
		el.style.setProperty(`border-${side}-width`, `${border}px`);
	}
	el.style.borderStyle = 'solid';
	el.getBoundingClientRect = () => box as DOMRect;
	document.body.appendChild(el);
	return el;
}

describe('contentBox', () => {
	it('excludes the padding and border the element keeps for itself', () => {
		expect(contentBox(inset({ width: 500, height: 300 }, 8, 1))).toEqual({
			width: 482,
			height: 282,
		});
	});

	it('never reports a negative box', () => {
		expect(contentBox(inset({ width: 10, height: 10 }, 40))).toEqual({ width: 0, height: 0 });
	});
});

describe('scaleToFit', () => {
	it('shrinks by whichever axis runs out first', () => {
		expect(scaleToFit({ width: 800, height: 400 }, { width: 200, height: 400 })).toBeCloseTo(0.25);
		expect(scaleToFit({ width: 800, height: 400 }, { width: 800, height: 100 })).toBeCloseTo(0.25);
	});

	it('never enlarges a grid smaller than its box', () => {
		// Letterboxed and crisp beats stretched and blurry; taking the pane's size is a click away.
		expect(scaleToFit({ width: 400, height: 200 }, { width: 1600, height: 900 })).toBe(1);
	});

	it('leaves the grid alone when either side cannot be measured', () => {
		expect(scaleToFit({ width: 0, height: 200 }, { width: 800, height: 400 })).toBe(1);
		expect(scaleToFit({ width: 400, height: 200 }, { width: 800, height: 0 })).toBe(1);
	});
});
