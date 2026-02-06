<script lang="ts">
import '../app.css';
import { onMount, onDestroy } from 'svelte';
import { ConnectionManager, connectionStore } from '$lib/stores/connection';
import { client, torenStore } from '$lib/stores/toren';

let manager: ConnectionManager;
let prevConnected = false;

onMount(() => {
	manager = new ConnectionManager();

	manager.onConnected = async () => {
		const state = $torenStore;
		await Promise.all([
			torenStore.loadSegments(state.shipUrl),
			torenStore.loadAssignments(state.shipUrl),
			torenStore.loadAncillaries(state.shipUrl),
		]);

		// Restore selected segment from localStorage
		const savedSegment = localStorage.getItem('toren_selected_segment');
		if (savedSegment) {
			try {
				const segment = JSON.parse(savedSegment);
				torenStore.selectSegment(segment);
			} catch (e) {
				console.error('Failed to restore selected segment:', e);
			}
		}
	};

	manager.onHeartbeat = async () => {
		const state = $torenStore;
		await Promise.all([
			torenStore.loadAncillaries(state.shipUrl),
			torenStore.loadAssignments(state.shipUrl),
		]);
	};

	manager.init();
});

onDestroy(() => {
	manager?.destroy();
});

// Watch for WS disconnect to notify ConnectionManager
$: {
	const connected = $torenStore.connected;
	if (prevConnected && !connected && $connectionStore.phase === 'connected') {
		manager?.notifyDisconnect();
	}
	prevConnected = connected;
}
</script>

<slot />
