<script lang="ts">
import type { BeadDisplayStatus } from '$lib/types/toren';

export let status: BeadDisplayStatus;
</script>

{#if status === 'open'}
	<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" aria-label="Open">
		<circle cx="6" cy="6" r="4.5" />
	</svg>
{:else if status === 'in_progress'}
	<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" aria-label="In progress">
		<circle cx="6" cy="6" r="4.5" />
		<path d="M6 1.5 A4.5 4.5 0 0 1 6 10.5" fill="currentColor" stroke="none" />
	</svg>
{:else if status === 'closed'}
	<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" aria-label="Closed">
		<circle cx="6" cy="6" r="5.25" fill="currentColor" />
	</svg>
{:else}
	<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" aria-label="Unknown">
		<path d="M6 1 L11 10 H1 Z" />
	</svg>
{/if}
