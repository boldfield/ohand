async function runEcho() {
  const input = document.getElementById('messageInput').value;
  const status = document.getElementById('status');
  const result = document.getElementById('result');
  const { invoke } = window.__TAURI__.core;

  try {
    status.textContent = 'Calling Rust backend...';
    result.textContent = await invoke('echo_message', { input });
    status.textContent = 'Success';
  } catch (error) {
    status.textContent = 'Error communicating with Rust backend';
    result.textContent = `Error: ${error}`;
  }

  // Report the text actually rendered so the simulator test can assert the
  // value that made it back into the UI, not just that Rust was called.
  await invoke('record_roundtrip', {
    status: status.textContent,
    renderedResult: result.textContent,
  });
}

document.addEventListener('DOMContentLoaded', () => {
  document.getElementById('echoButton').addEventListener('click', runEcho);
  runEcho();
});
