/**
 * A robust clipboard copy utility function
 * 
 * Browser limitation: navigator.clipboard is undefined in a non-secure context (non-HTTPS or non-localhost).
 * This function provides a fallback via execCommand('copy'), ensuring it also works in HTTP environments (e.g. Docker IP access).
 */
export async function copyToClipboard(source: string | (() => Promise<string>)): Promise<boolean> {
    let pendingText: Promise<string> | undefined;
    const resolveText = () => {
        // One retrieval even when a clipboard implementation needs a fallback.
        pendingText ??= typeof source === 'string' ? Promise.resolve(source) : Promise.resolve().then(source);
        return pendingText;
    };

    // WebKit requires write() during the click itself. Defer the key retrieval
    // through the ClipboardItem's promise rather than awaiting it before write.
    if (typeof source === 'function' && navigator.clipboard && window.isSecureContext
        && typeof navigator.clipboard.write === 'function' && typeof ClipboardItem !== 'undefined') {
        try {
            const content = resolveText().then(text => new Blob([text], { type: 'text/plain' }));
            // A permission/constructor failure may occur before the browser reads
            // this promise. Mark its rejection handled without hiding it from write().
            void content.catch(() => {});
            await navigator.clipboard.write([new ClipboardItem({ 'text/plain': content })]);
            return true;
        } catch {
            console.error('Deferred clipboard copy failed');
        }
    }

    let text: string;
    try {
        text = typeof source === 'string' ? source : await resolveText();
    } catch {
        console.error('Clipboard content retrieval failed');
        return false;
    }

    // 1. Try the modern Clipboard API
    if (navigator.clipboard && window.isSecureContext) {
        try {
            await navigator.clipboard.writeText(text);
            return true;
        } catch {
            console.error('Clipboard API copy failed');
        }
    }

    // 2. Fall back to the legacy execCommand('copy') approach
    let textArea: HTMLTextAreaElement | undefined;
    try {
        textArea = document.createElement('textarea');
        textArea.value = text;

        // Ensure the textarea is invisible on the page, but it must be in the DOM for copy to work
        textArea.style.position = 'fixed';
        textArea.style.left = '-9999px';
        textArea.style.top = '0';
        document.body.appendChild(textArea);

        textArea.focus();
        textArea.select();

        return document.execCommand('copy');
    } catch {
        console.error('execCommand copy failed');
        return false;
    } finally {
        if (textArea) {
            textArea.value = '';
            textArea.remove();
        }
    }
}
