import type { Terminal } from '@xterm/xterm';

/** A rectangle in CSS pixels. */
export interface Box {
	width: number;
	height: number;
}

/** The size of one character cell, as the renderer is currently drawing it. */
export interface CellSize {
	width: number;
	height: number;
}

export interface Geometry {
	cols: number;
	rows: number;
}

/** xterm's own floor, and a terminal thinner than this is unusable anyway. */
export const MIN_COLS = 2;
export const MIN_ROWS = 1;

/** The strip the pane's scrollbar draws in, kept clear of text. */
export const SCROLLBAR_WIDTH = 14;

/**
 * How many whole cells fit in `box`.
 *
 * Whole ones only: a terminal paints exactly `rows * cell.height` pixels of grid, so a row that
 * only partly fits is a row with its bottom sliced off by whatever clips the container. The pixels
 * left over below the last row are the container's, not the terminal's.
 */
export function proposeGeometry(
	box: Box,
	cell: CellSize,
	scrollbarWidth = SCROLLBAR_WIDTH,
): Geometry | null {
	if (!(cell.width > 0) || !(cell.height > 0)) return null;
	if (!(box.width > 0) || !(box.height > 0)) return null;
	const width = box.width - scrollbarWidth;
	if (!(width > 0)) return null;
	return {
		cols: Math.max(MIN_COLS, Math.floor(width / cell.width)),
		rows: Math.max(MIN_ROWS, Math.floor(box.height / cell.height)),
	};
}

/**
 * The box an element's children are laid out in: its border box, less its own border and padding.
 *
 * Measured rather than read off `getComputedStyle().height`, which reports the *border* box for a
 * `box-sizing: border-box` element — padding included, which is a box nothing is drawn in.
 */
export function contentBox(el: HTMLElement): Box {
	const rect = el.getBoundingClientRect();
	const style = getComputedStyle(el);
	const horizontal =
		px(style.paddingLeft) +
		px(style.paddingRight) +
		px(style.borderLeftWidth) +
		px(style.borderRightWidth);
	const vertical =
		px(style.paddingTop) +
		px(style.paddingBottom) +
		px(style.borderTopWidth) +
		px(style.borderBottomWidth);
	return {
		width: Math.max(0, rect.width - horizontal),
		height: Math.max(0, rect.height - vertical),
	};
}

function px(value: string): number {
	const parsed = Number.parseFloat(value);
	return Number.isFinite(parsed) ? parsed : 0;
}

/**
 * The cell size the renderer settled on, which is not the font size: it is measured from the font
 * once it has actually loaded, and it is the only thing that turns pixels into rows.
 */
export function cellSize(term: Terminal): CellSize | null {
	const cell = (term as unknown as XtermInternals)._core?._renderService?.dimensions?.css?.cell;
	if (!cell || !(cell.width > 0) || !(cell.height > 0)) return null;
	return { width: cell.width, height: cell.height };
}

/** The renderer's measurements are not on the public surface; nothing else reports them. */
interface XtermInternals {
	_core?: {
		_renderService?: {
			dimensions?: { css?: { cell?: CellSize } };
		};
	};
}

/**
 * Size `term` to the whole cells that fit in `host`, and report what it settled on.
 *
 * Null when the terminal cannot be measured yet — before the first render, or while the host is
 * laid out at zero height — in which case the terminal keeps the size it has.
 */
export function fitTerminal(term: Terminal, host: HTMLElement): Geometry | null {
	const cell = cellSize(term);
	if (!cell) return null;
	const scrollbar = term.options.scrollback === 0 ? 0 : SCROLLBAR_WIDTH;
	const geometry = proposeGeometry(contentBox(host), cell, scrollbar);
	if (!geometry) return null;
	if (geometry.cols !== term.cols || geometry.rows !== term.rows) {
		term.resize(geometry.cols, geometry.rows);
	}
	return geometry;
}
