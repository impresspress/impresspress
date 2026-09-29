// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_status_poll.test.mjs
//
// The status poll's in-flight guard. See `harness.mjs` for how the tail is
// loaded without adding a test hook to the shipped file.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

/** How many `/b/dev/api/status` requests the tail has made so far. */
const statusCalls = (fetchCalls) =>
  fetchCalls.filter(([url]) => String(url) === '/b/dev/api/status').length;

/** Let every already-queued microtask run. */
const flush = async () => {
  for (let i = 0; i < 32; i += 1) {
    await Promise.resolve();
  }
};

// Why this matters beyond tidiness: `/b/dev/api/status` reads the workspace
// manifest under `DevShared::workspace` (`blocks/dev/gc.rs::storage_usage`),
// and the collector holds that mutex across a loop of sequential deletes. The
// mutex is first-in-first-out, so an ungated poll firing every 300 ms through
// a collector pass puts one waiter per tick ahead of whatever the user does
// next. The guard makes the pass cost one waiter.
test('a poll tick while a status request is outstanding issues nothing', async () => {
  let releaseStatus;
  const statusGate = new Promise((r) => (releaseStatus = r));
  const { handle, fetchCalls, fireInterval } = instantiate({ statusGate });

  // Polling only runs while a mutating call is outstanding, so one is opened
  // here and held for the duration of the test.
  let releaseCall;
  const callGate = new Promise((r) => (releaseCall = r));
  const call = handle.withProgress(async () => callGate);
  const pending = call();
  await flush();
  assert.equal(handle.isPolling, true, 'an outstanding call must start the poll');

  const before = statusCalls(fetchCalls);
  fireInterval();
  await flush();
  assert.equal(handle.statusInFlight, true, 'the first tick is in flight');
  assert.equal(statusCalls(fetchCalls) - before, 1, 'the first tick issues one request');

  fireInterval();
  fireInterval();
  await flush();
  assert.equal(
    statusCalls(fetchCalls) - before,
    1,
    'two more ticks while the first is outstanding must issue nothing'
  );

  releaseStatus();
  await flush();
  assert.equal(handle.statusInFlight, false, 'the guard clears when the answer arrives');

  fireInterval();
  await flush();
  assert.equal(
    statusCalls(fetchCalls) - before,
    2,
    'the next tick after the answer arrives does issue a request'
  );

  releaseCall();
  await pending;
});

// The failure a guard invites: a flag set on the way out and cleared only on
// success stops the panel updating for the rest of the session the first time
// the endpoint refuses. The tail clears it in a `then` placed AFTER the
// `catch`, which is what this pins.
test('a refused status request clears the guard, so polling recovers', async () => {
  let refuse;
  const statusGate = new Promise((_resolve, reject) => (refuse = reject));
  const { handle, fetchCalls, fireInterval } = instantiate({ statusGate });

  let releaseCall;
  const callGate = new Promise((r) => (releaseCall = r));
  const call = handle.withProgress(async () => callGate);
  const pending = call();
  await flush();

  const before = statusCalls(fetchCalls);
  fireInterval();
  await flush();
  assert.equal(handle.statusInFlight, true);

  refuse(new Error('the sandbox refused the status read'));
  await flush();
  assert.equal(handle.statusInFlight, false, 'a refusal must clear the guard too');

  fireInterval();
  await flush();
  assert.equal(
    statusCalls(fetchCalls) - before,
    2,
    'the poll must keep polling after a refusal'
  );

  releaseCall();
  await pending;
});
