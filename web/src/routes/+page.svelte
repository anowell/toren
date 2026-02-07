<script lang="ts">
import { goto } from '$app/navigation';
import PairingModal from '$lib/components/PairingModal.svelte';
import SegmentSelector from '$lib/components/SegmentSelector.svelte';
import { torenStore } from '$lib/stores/toren';

let showSegmentSelector = false;

// Auto-show segment selector when authenticated but no segment
$: if ($torenStore.authenticated && !$torenStore.selectedSegment && !showSegmentSelector) {
	showSegmentSelector = true;
}

// Navigate to segment route when segment is selected
$: if ($torenStore.selectedSegment) {
	goto(`/a/${$torenStore.selectedSegment.name.toLowerCase()}`);
}

function toggleSegmentSelector() {
	showSegmentSelector = !showSegmentSelector;
}
</script>

<svelte:head>
	<title>Toren - Mobile-First Development Intelligence</title>
</svelte:head>

<PairingModal />

{#if $torenStore.authenticated}
	{#if showSegmentSelector || !$torenStore.selectedSegment}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<div class="overlay" on:click={toggleSegmentSelector} role="presentation">
			<div class="selector-container" on:click|stopPropagation role="dialog" tabindex="-1">
				<SegmentSelector />
			</div>
		</div>
	{/if}
{:else}
	<!-- Show landing/welcome when not authenticated -->
	<div class="landing">
		<div class="landing-content">
			<h1>Toren</h1>
			<p>Mobile-first development intelligence</p>
			<p class="hint">Connect to a Toren daemon to get started</p>
		</div>
	</div>
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

	.landing {
		display: flex;
		align-items: center;
		justify-content: center;
		height: 100vh;
		background: var(--color-bg);
	}

	.landing-content {
		text-align: center;
		padding: var(--spacing-xl);
	}

	.landing h1 {
		font-size: 3rem;
		color: var(--color-primary);
		margin: 0 0 var(--spacing-md) 0;
	}

	.landing p {
		color: var(--color-text-secondary);
		margin: 0 0 var(--spacing-sm) 0;
	}

	.landing .hint {
		font-size: 0.85rem;
	}

	@media (min-width: 768px) {
		.overlay {
			align-items: center;
			justify-content: center;
			background: rgba(0, 0, 0, 0.8);
		}

		.selector-container {
			max-width: 500px;
			max-height: 600px;
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
