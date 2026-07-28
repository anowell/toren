import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vitest/config';

export default defineConfig({
	plugins: [sveltekit()],
	// Components are mounted, not server-rendered, so svelte has to resolve to its browser build.
	resolve: { conditions: ['browser'] },
	test: {
		globals: true,
		environment: 'happy-dom',
		setupFiles: ['./src/test/setup.ts'],
	},
});
