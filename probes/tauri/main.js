// Tauri 2 iOS probe: test native round-trip communication
async function testEcho() {
    const input = document.getElementById('messageInput').value;
    const status = document.getElementById('status');
    const result = document.getElementById('result');

    if (!input.trim()) {
        result.textContent = 'Error: Input cannot be empty';
        return;
    }

    try {
        status.textContent = 'Calling Rust backend...';

        const response = await window.__TAURI__.core.invoke('echo_message', { input });

        status.textContent = 'Success';
        result.textContent = response;
    } catch (error) {
        status.textContent = 'Error communicating with Rust backend';
        result.textContent = `Error: ${error}`;
        console.error('Tauri command failed:', error);
    }
}

// Initialize when Tauri is ready
document.addEventListener('DOMContentLoaded', () => {
    const statusEl = document.getElementById('status');
    const echoButton = document.getElementById('echoButton');

    if (echoButton) {
        echoButton.addEventListener('click', testEcho);
    }

    if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.core) {
        statusEl.textContent = 'Tauri ready - running automatic echo test...';
        // Automatically run the echo test on load for CI verification
        setTimeout(testEcho, 500);
    } else {
        statusEl.textContent = 'Waiting for Tauri...';
        // Retry after a delay in case Tauri isn't loaded yet
        setTimeout(() => {
            if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.core) {
                statusEl.textContent = 'Tauri ready - running automatic echo test...';
                setTimeout(testEcho, 500);
            }
        }, 1000);
    }
});
