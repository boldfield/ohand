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

// The native side stores handoffs without this page. The page only reads identifiers back.
async function refreshHandoffs() {
  const summary = document.getElementById('handoff-summary');
  const list = document.getElementById('handoff-list');
  const { invoke } = window.__TAURI__.core;

  try {
    const snapshot = await invoke('list_handoffs');
    const items = snapshot.captureIds.map((captureId) => {
      const item = document.createElement('li');
      item.textContent = captureId;
      return item;
    });
    list.replaceChildren(...items);
    const received = snapshot.captureIds.length;
    summary.textContent =
      received === 0 && snapshot.rejectedCount === 0
        ? 'No captures received yet.'
        : `Captures received: ${received}. Handoffs rejected: ${snapshot.rejectedCount}.`;
  } catch (error) {
    summary.textContent = `Could not read received captures: ${error}`;
  }
}

document.addEventListener('DOMContentLoaded', () => {
  document.getElementById('echoButton').addEventListener('click', runEcho);
  document.getElementById('refreshHandoffs').addEventListener('click', refreshHandoffs);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') {
      refreshHandoffs();
    }
  });
  setInterval(refreshHandoffs, 2000);
  refreshHandoffs();
  runEcho();
});
