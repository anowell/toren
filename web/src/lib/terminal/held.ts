/**
 * The keys a held pane answers to.
 *
 * A pane created to hold outlives its process, and the mirror draws its exit status into the byte
 * stream — `[exited 0 — <ENTER> re-run, <ESC> drop to shell, <Ctrl-c> close]`. The line is the
 * same everywhere the pane is shown; only what the three keys do is per-surface, and this is the
 * browser's half of it.
 */

export type HeldAction =
	/** `<ENTER>`: run it again, or resume the agent session the pane was working. */
	| 'primary'
	/** `<ESC>`: leave the dead pane for a live shell. */
	| 'shell'
	/** `<Ctrl-c>`: dismiss the pane. */
	| 'close';

/**
 * Which affordance a keystroke reaches for, if any.
 *
 * A lone `ESC` is the drop-to-shell key; an `ESC` that opens a longer sequence is an arrow key or
 * a mouse report, which a pane with nothing running in it has no use for.
 */
export function heldAction(data: string): HeldAction | null {
	if (data === '\r' || data === '\n') return 'primary';
	if (data === '\x1b') return 'shell';
	if (data === '\x03') return 'close';
	return null;
}
