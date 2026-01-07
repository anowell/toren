<script lang="ts">
	import ChatInterface from '$lib/components/ChatInterface.svelte';
	import PairingModal from '$lib/components/PairingModal.svelte';
	import SegmentSelector from '$lib/components/SegmentSelector.svelte';
	import { torenStore } from '$lib/stores/toren';

	let showSegmentSelector = false;

	$: {
		// Show segment selector if connected and authenticated but no segment selected
		if ($torenStore.authenticated && !$torenStore.selectedSegment && !showSegmentSelector) {
			showSegmentSelector = true;
		}
		// Hide segment selector once a segment is selected
		if ($torenStore.selectedSegment && showSegmentSelector) {
			showSegmentSelector = false;
		}
	}

	function toggleSegmentSelector() {
		showSegmentSelector = !showSegmentSelector;
	}
</script>

<svelte:head>
	<title>Toren - Mobile-First Development Intelligence</title>
</svelte:head>

<PairingModal />

{#if showSegmentSelector}
	<div class="overlay" on:click={toggleSegmentSelector} role="presentation">
		<div class="selector-container" on:click|stopPropagation role="dialog">
			<SegmentSelector />
		</div>
	</div>
{:else}
	<ChatInterface />
	{#if $torenStore.authenticated}
		<button class="fab" on:click={toggleSegmentSelector} aria-label="Change project">
			<svg
				xmlns="http://www.w3.org/2000/svg"
				width="24"
				height="24"
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path>
			</svg>
		</button>
	{/if}
{/if}

<style>
	.overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.5);
		z-index: 100;
		display: flex;
		align-items: flex-end;
	}

	.selector-container {
		width: 100%;
		max-height: 80vh;
		background: var(--color-bg);
		border-radius: var(--radius-lg) var(--radius-lg) 0 0;
		overflow: hidden;
		animation: slideUp 0.3s ease-out;
	}

	@keyframes slideUp {
		from {
			transform: translateY(100%);
		}
		to {
			transform: translateY(0);
		}
	}

	.fab {
		position: fixed;
		bottom: var(--spacing-lg);
		right: var(--spacing-lg);
		width: 56px;
		height: 56px;
		border-radius: 50%;
		background: var(--color-primary);
		color: white;
		display: flex;
		align-items: center;
		justify-content: center;
		box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
		transition: all 0.2s;
		z-index: 50;
	}

	.fab:hover {
		background: var(--color-primary-hover);
		transform: scale(1.05);
		box-shadow: 0 6px 16px rgba(0, 0, 0, 0.4);
	}

	.fab:active {
		transform: scale(0.95);
	}

	@media (min-width: 768px) {
		.overlay {
			align-items: center;
			justify-content: center;
		}

		.selector-container {
			max-width: 600px;
			max-height: 700px;
			border-radius: var(--radius-lg);
			animation: scaleIn 0.3s ease-out;
		}

		@keyframes scaleIn {
			from {
				transform: scale(0.9);
				opacity: 0;
			}
			to {
				transform: scale(1);
				opacity: 1;
			}
		}
	}
</style>
