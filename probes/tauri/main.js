// Tauri iOS probe: test native round-trip communication
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

        // Note: At this phase, we test the Tauri framework communication capability.
        // Production provider keys would NOT be passed to JavaScript per the spec.
        // This probe verifies that the webview can call native Rust code successfully.
        const response = await window.__TAURI__.invoke('echo_message', { input });

        status.textContent = 'Success';
        result.textContent = response;
    } catch (error) {
        status.textContent = 'Error communicating with Rust backend';
        result.textContent = `Error: ${error}`;
        console.error('Tauri command failed:', error);
    }
}

// Initialize when Tauri is ready
if (typeof window.__TAURI__ !== 'undefined') {
    document.getElementById('status').textContent = 'Tauri ready';
} else {
    document.getElementById('status').textContent = 'Waiting for Tauri...';
}
