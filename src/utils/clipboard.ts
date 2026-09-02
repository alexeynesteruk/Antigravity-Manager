/**
 * A robust clipboard copy utility function
 * 
 * Browser limitation: navigator.clipboard is undefined in a non-secure context (non-HTTPS or non-localhost).
 * This function provides a fallback via execCommand('copy'), ensuring it also works in HTTP environments (e.g. Docker IP access).
 */
export async function copyToClipboard(text: string): Promise<boolean> {
    // 1. Try the modern Clipboard API
    if (navigator.clipboard && window.isSecureContext) {
        try {
            await navigator.clipboard.writeText(text);
            return true;
        } catch (err) {
            console.error('Clipboard API copy failed:', err);
        }
    }

    // 2. Fall back to the legacy execCommand('copy') approach
    try {
        const textArea = document.createElement('textarea');
        textArea.value = text;

        // Ensure the textarea is invisible on the page, but it must be in the DOM for copy to work
        textArea.style.position = 'fixed';
        textArea.style.left = '-9999px';
        textArea.style.top = '0';
        document.body.appendChild(textArea);

        textArea.focus();
        textArea.select();

        const successful = document.execCommand('copy');
        document.body.removeChild(textArea);

        return successful;
    } catch (err) {
        console.error('execCommand copy failed:', err);
        return false;
    }
}
