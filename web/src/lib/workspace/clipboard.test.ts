import { afterEach, describe, expect, it, vi } from 'vitest';
import { copyText } from './clipboard';

const realClipboard = Object.getOwnPropertyDescriptor(navigator, 'clipboard');

function setClipboard(clipboard: unknown) {
	Object.defineProperty(navigator, 'clipboard', { value: clipboard, configurable: true });
}

afterEach(() => {
	if (realClipboard) Object.defineProperty(navigator, 'clipboard', realClipboard);
	else setClipboard(undefined);
	delete (document as { execCommand?: unknown }).execCommand;
});

describe('copyText', () => {
	it('writes through the clipboard api when the page has one', async () => {
		const writeText = vi.fn().mockResolvedValue(undefined);
		setClipboard({ writeText });
		expect(await copyText('breq sh one')).toBe(true);
		expect(writeText).toHaveBeenCalledWith('breq sh one');
	});

	it('falls back to a selection when the origin is not a secure context', async () => {
		setClipboard(undefined);
		const execCommand = vi.fn().mockReturnValue(true);
		(document as { execCommand?: unknown }).execCommand = execCommand;
		expect(await copyText('breq sh one')).toBe(true);
		expect(execCommand).toHaveBeenCalledWith('copy');
		expect(document.querySelector('textarea')).toBeNull();
	});

	it('falls back when the clipboard api refuses', async () => {
		setClipboard({ writeText: vi.fn().mockRejectedValue(new Error('denied')) });
		(document as { execCommand?: unknown }).execCommand = vi.fn().mockReturnValue(true);
		expect(await copyText('breq sh one')).toBe(true);
	});

	it('reports failure when neither way to copy exists', async () => {
		setClipboard(undefined);
		expect(await copyText('breq sh one')).toBe(false);
	});
});
