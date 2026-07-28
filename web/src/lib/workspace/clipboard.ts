/**
 * Copying text from a page that is not a secure context.
 *
 * `navigator.clipboard` does not exist on a plain-http origin, which is how this UI is reached from
 * a phone on the LAN — the case the attach commands exist for. The deprecated selection copy still
 * works there, so it is the fallback, and a refusal is reported rather than swallowed.
 */

export async function copyText(text: string): Promise<boolean> {
	if (navigator.clipboard?.writeText) {
		try {
			await navigator.clipboard.writeText(text);
			return true;
		} catch {
			// A clipboard that refused is still worth a second attempt through a selection.
		}
	}
	return copyBySelection(text);
}

function copyBySelection(text: string): boolean {
	if (typeof document.execCommand !== 'function') return false;
	const field = document.createElement('textarea');
	field.value = text;
	field.readOnly = true;
	field.setAttribute('aria-hidden', 'true');
	field.style.position = 'fixed';
	field.style.opacity = '0';
	field.style.top = '0';
	document.body.appendChild(field);
	field.select();
	field.setSelectionRange(0, text.length);
	let copied = false;
	try {
		copied = document.execCommand('copy');
	} catch {
		copied = false;
	}
	field.remove();
	return copied;
}
