<script lang="ts">
import { torenStore } from '$lib/stores/toren';
import type { Segment } from '$lib/types/toren';

let isOpen = false;
let dropdownRef: HTMLDivElement;

function toggleDropdown() {
	isOpen = !isOpen;
}

function selectSegment(segment: Segment) {
	torenStore.selectSegment(segment);
	isOpen = false;
}

function handleClickOutside(event: MouseEvent) {
	if (dropdownRef && !dropdownRef.contains(event.target as Node)) {
		isOpen = false;
	}
}

function getSegmentIcon(source: string): string {
	switch (source) {
		case 'glob':
			return '\u{1F4C1}'; // folder
		case 'path':
			return '\u{1F4CD}'; // pin
		case 'root':
			return '\u{2728}'; // sparkle
		default:
			return '\u{1F4C2}'; // file folder
	}
}
</script>

<svelte:window on:click={handleClickOutside} />

<div class="segment-dropdown" bind:this={dropdownRef}>
	<button class="dropdown-trigger" on:click={toggleDropdown} aria-expanded={isOpen}>
		<svg
			xmlns="http://www.w3.org/2000/svg"
			width="14"
			height="14"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
		>
			<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path>
		</svg>
		<span class="segment-name">{$torenStore.selectedSegment?.name || 'Select Segment'}</span>
		<svg
			class="chevron"
			class:open={isOpen}
			xmlns="http://www.w3.org/2000/svg"
			width="12"
			height="12"
			viewBox="0 0 24 24"
			fill="none"
			stroke="currentColor"
			stroke-width="2"
			stroke-linecap="round"
			stroke-linejoin="round"
		>
			<polyline points="6 9 12 15 18 9"></polyline>
		</svg>
	</button>

	{#if isOpen}
		<div class="dropdown-menu">
			{#if $torenStore.loadingSegments}
				<div class="loading">Loading...</div>
			{:else if $torenStore.segments.length === 0}
				<div class="empty">No segments available</div>
			{:else}
				{#each $torenStore.segments as segment (segment.path)}
					<button
						class="dropdown-item"
						class:selected={$torenStore.selectedSegment?.path === segment.path}
						on:click={() => selectSegment(segment)}
					>
						<span class="item-icon">{getSegmentIcon(segment.source)}</span>
						<span class="item-name">{segment.name}</span>
						{#if $torenStore.selectedSegment?.path === segment.path}
							<svg
								class="check"
								xmlns="http://www.w3.org/2000/svg"
								width="16"
								height="16"
								viewBox="0 0 24 24"
								fill="none"
								stroke="currentColor"
								stroke-width="2"
								stroke-linecap="round"
								stroke-linejoin="round"
							>
								<polyline points="20 6 9 17 4 12"></polyline>
							</svg>
						{/if}
					</button>
				{/each}
			{/if}
		</div>
	{/if}
</div>

<style>
	.segment-dropdown {
		position: relative;
	}

	.dropdown-trigger {
		display: flex;
		align-items: center;
		gap: var(--spacing-xs);
		padding: var(--spacing-xs) var(--spacing-sm);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		color: var(--color-text-secondary);
		font-size: 0.85rem;
		cursor: pointer;
		transition: all 0.15s ease;
	}

	.dropdown-trigger:hover {
		border-color: var(--color-primary);
		color: var(--color-text);
	}

	.segment-name {
		max-width: 150px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chevron {
		transition: transform 0.15s ease;
		opacity: 0.7;
	}

	.chevron.open {
		transform: rotate(180deg);
	}

	.dropdown-menu {
		position: absolute;
		top: calc(100% + var(--spacing-xs));
		left: 0;
		min-width: 200px;
		max-width: 300px;
		max-height: 300px;
		overflow-y: auto;
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
		z-index: 100;
		animation: fadeIn 0.15s ease;
	}

	@keyframes fadeIn {
		from {
			opacity: 0;
			transform: translateY(-4px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}

	.loading,
	.empty {
		padding: var(--spacing-md);
		text-align: center;
		color: var(--color-text-secondary);
		font-size: 0.85rem;
	}

	.dropdown-item {
		display: flex;
		align-items: center;
		gap: var(--spacing-sm);
		width: 100%;
		padding: var(--spacing-sm) var(--spacing-md);
		background: none;
		border: none;
		color: var(--color-text);
		font-size: 0.9rem;
		text-align: left;
		cursor: pointer;
		transition: background-color 0.15s ease;
	}

	.dropdown-item:hover {
		background: var(--color-bg-tertiary);
	}

	.dropdown-item.selected {
		background: var(--color-bg-tertiary);
	}

	.item-icon {
		font-size: 1rem;
		flex-shrink: 0;
	}

	.item-name {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.check {
		color: var(--color-primary);
		flex-shrink: 0;
	}
</style>
