import { cleanup } from '@testing-library/svelte';
import { afterEach, expect } from 'vitest';
import '@testing-library/jest-dom/vitest';

afterEach(() => {
	cleanup();
});
