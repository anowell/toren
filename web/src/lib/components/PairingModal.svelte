<script lang="ts">
	import { torenStore, client } from '$lib/stores/toren';

	let pairingToken = '';
	let shipUrl = 'http://localhost:8787';
	let pairing = false;
	let error = '';

	async function handlePair() {
		error = '';
		pairing = true;

		try {
			// First, call the /pair REST endpoint
			const response = await fetch(`${shipUrl}/pair`, {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ pairing_token: pairingToken }),
			});

			if (!response.ok) {
				throw new Error('Pairing failed');
			}

			const data = await response.json();

			// Update store with ship URL
			torenStore.update((state) => ({
				...state,
				shipUrl,
				sessionToken: data.session_token,
			}));

			// Connect WebSocket
			torenStore.update((state) => ({ ...state, connecting: true }));
			await client.connect(shipUrl);

			// Authenticate
			await client.authenticate(data.session_token);

			// Store session token in localStorage
			localStorage.setItem('toren_session_token', data.session_token);
			localStorage.setItem('toren_ship_url', shipUrl);

			// Load segments
			await torenStore.loadSegments(shipUrl);

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
		} catch (err) {
			error = err instanceof Error ? err.message : 'Pairing failed';
			console.error('Pairing error:', err);
		} finally {
			pairing = false;
		}
	}

	// Try to auto-connect with stored credentials
	$: if (typeof window !== 'undefined') {
		const storedToken = localStorage.getItem('toren_session_token');
		const storedUrl = localStorage.getItem('toren_ship_url');

		if (storedToken && storedUrl && !$torenStore.connected && !$torenStore.connecting) {
			shipUrl = storedUrl;
			client
				.connect(storedUrl)
				.then(() => client.authenticate(storedToken))
				.then(() => torenStore.loadSegments(storedUrl))
				.then(() => {
					// Restore selected segment
					const savedSegment = localStorage.getItem('toren_selected_segment');
					if (savedSegment) {
						try {
							const segment = JSON.parse(savedSegment);
							torenStore.selectSegment(segment);
						} catch (e) {
							console.error('Failed to restore selected segment:', e);
						}
					}
				})
				.catch((err) => {
					console.log('Auto-connect failed:', err);
					// Clear stored credentials
					localStorage.removeItem('toren_session_token');
					localStorage.removeItem('toren_ship_url');
				});
		}
	}
</script>

{#if !$torenStore.authenticated && !$torenStore.connecting}
	<div class="modal-overlay">
		<div class="modal">
			<h2>Connect to Toren</h2>
			<p class="subtitle">Enter your pairing token to get started</p>

			<form on:submit|preventDefault={handlePair}>
				<div class="form-group">
					<label for="shipUrl">Toren URL</label>
					<input
						type="text"
						id="shipUrl"
						bind:value={shipUrl}
						placeholder="http://localhost:8787"
						disabled={pairing}
					/>
				</div>

				<div class="form-group">
					<label for="pairingToken">Pairing Token</label>
					<input
						type="text"
						id="pairingToken"
						bind:value={pairingToken}
						placeholder="Enter 6-digit token"
						disabled={pairing}
						autocomplete="off"
						required
					/>
				</div>

				{#if error}
					<div class="error">{error}</div>
				{/if}

				<button type="submit" disabled={pairing || !pairingToken}>
					{pairing ? 'Connecting...' : 'Connect'}
				</button>
			</form>

			<div class="help">
				<p>Get your pairing token by running:</p>
				<code>just daemon</code>
			</div>
		</div>
	</div>
{/if}

<style>
	.modal-overlay {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.8);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: 1000;
		padding: var(--spacing-md);
	}

	.modal {
		background: var(--color-bg-secondary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
		padding: var(--spacing-xl);
		max-width: 400px;
		width: 100%;
	}

	h2 {
		margin: 0 0 var(--spacing-sm) 0;
		color: var(--color-primary);
	}

	.subtitle {
		margin: 0 0 var(--spacing-lg) 0;
		color: var(--color-text-secondary);
		font-size: 0.9rem;
	}

	.form-group {
		margin-bottom: var(--spacing-md);
	}

	label {
		display: block;
		margin-bottom: var(--spacing-xs);
		color: var(--color-text);
		font-size: 0.9rem;
		font-weight: 500;
	}

	input {
		width: 100%;
		padding: var(--spacing-sm) var(--spacing-md);
		background: var(--color-bg-tertiary);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		font-size: 1rem;
		transition: border-color 0.2s;
	}

	input:focus {
		border-color: var(--color-primary);
	}

	input:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	button {
		width: 100%;
		padding: var(--spacing-md);
		background: var(--color-primary);
		color: white;
		border-radius: var(--radius-md);
		font-size: 1rem;
		font-weight: 600;
		transition: background-color 0.2s;
	}

	button:hover:not(:disabled) {
		background: var(--color-primary-hover);
	}

	button:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}

	.error {
		margin-bottom: var(--spacing-md);
		padding: var(--spacing-sm) var(--spacing-md);
		background: rgba(248, 113, 113, 0.1);
		border: 1px solid var(--color-error);
		border-radius: var(--radius-sm);
		color: var(--color-error);
		font-size: 0.9rem;
	}

	.help {
		margin-top: var(--spacing-lg);
		padding-top: var(--spacing-lg);
		border-top: 1px solid var(--color-border);
	}

	.help p {
		margin: 0 0 var(--spacing-sm) 0;
		font-size: 0.85rem;
		color: var(--color-text-secondary);
	}

	code {
		display: block;
		padding: var(--spacing-sm);
		background: var(--color-bg-tertiary);
		border-radius: var(--radius-sm);
		font-family: var(--font-mono);
		font-size: 0.9rem;
		color: var(--color-success);
	}
</style>
