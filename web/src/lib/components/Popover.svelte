<script lang="ts">
import { createEventDispatcher } from 'svelte';

// biome-ignore-start lint/style/useConst: svelte props are reassigned by the parent
/** Extra classes for the chip itself, so each fact can carry its own colouring. */
export let chipClass = '';
/** The chip's tooltip — the sentence a glyph is standing in for. */
export let title = '';
/** What the chip is called to a screen reader, when its glyphs do not say. */
export let label = '';
// biome-ignore-end lint/style/useConst: svelte props are reassigned by the parent

const dispatch = createEventDispatcher<{ open: null; close: null }>();

let open = false;
let host: HTMLDivElement;
/** Where the panel sits on screen. Fixed, so a bar that scrolls sideways cannot clip it. */
let placement = '';

function toggle() {
	if (open) {
		close();
		return;
	}
	place();
	open = true;
	dispatch('open', null);
}

function close() {
	if (!open) return;
	open = false;
	dispatch('close', null);
}

/** Below the chip and aligned with it, pulled back from the edge when it would hang off. */
function place() {
	const chip = host?.getBoundingClientRect();
	if (!chip) return;
	const width = Math.min(340, window.innerWidth - 16);
	const left = Math.max(8, Math.min(chip.left, window.innerWidth - width - 8));
	placement = `top: ${chip.bottom + 4}px; left: ${left}px; width: ${width}px;`;
}

/** A click anywhere outside dismisses; a click on the chip is its own toggle. */
function handleWindowClick(event: MouseEvent) {
	if (open && host && !host.contains(event.target as Node)) close();
}

function handleKeydown(event: KeyboardEvent) {
	if (event.key === 'Escape') close();
}
</script>

<svelte:window on:click={handleWindowClick} on:keydown={handleKeydown} on:resize={close} />

<div class="popover-host" bind:this={host}>
	<button
		class="fact-chip {chipClass}"
		class:open
		on:click={toggle}
		aria-expanded={open}
		aria-haspopup="dialog"
		aria-label={label || undefined}
		{title}
	>
		<slot name="chip" />
	</button>
	{#if open}
		<div class="popover" style={placement} role="dialog" aria-label={label || title}>
			<slot {close} />
		</div>
	{/if}
</div>

<style>
	.popover-host {
		display: inline-flex;
		flex-shrink: 0;
	}

	.fact-chip.open {
		border-color: var(--color-primary);
		color: var(--color-text);
	}

	.popover {
		position: fixed;
		z-index: 200;
		max-height: 60vh;
		overflow-y: auto;
		padding: var(--spacing-sm);
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
		font-size: 0.8rem;
	}
</style>
