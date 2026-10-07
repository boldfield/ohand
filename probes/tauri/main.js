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
    if (typeof window.__TAURI__ !== 'undefined' && window.__TAURI__.core) {
        document.getElementById('status').textContent = 'Tauri ready';
    } else {
        document.getElementById('status').textContent = 'Waiting for Tauri...';
    }
});
