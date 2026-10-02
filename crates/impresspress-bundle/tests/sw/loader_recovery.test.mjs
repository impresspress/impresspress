// What the rendered boot loader does with the cause of a dead runtime.
//
// Three things here guard something that cannot be taken back or that never
// ends: the recovery may ERASE the app's local data (so it must only do that
// for a failure the worker reported, and one that is about this load), it
// must run automatically at most once per failure (so the cause stays on
// screen instead of looping), and a boot probe the runtime dies on must not
// be mistaken for a boot that worked.
//
// Every road into the recovery, and what it may do to local data (OPFS) in a
// build rendered with `opfs_wipe_on_recovery`:
//
//   the worker's `sw-self-destruct` message (the breaker)   reported   erases
//   the cause the worker left in Cache Storage, fresh        reported   erases
//   the worker's 503 to the boot probe                       reported   erases
//   a cause entry that is stale, undated or malformed        —          no recovery
//   the boot probe ran out of time (either branch)           no cause   restarts, keeps data
//   the boot probe could not be made (it threw)              no cause   no recovery
//   the worker could not be registered                       no cause   no recovery
//   "Try again" on the stuck screen                          as the failure it shows
//   "Reset local data and reload"                            the person's choice: erases
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  BREAKER,
  loadShell,
  ORIGIN,
  RECOVERY_DONE,
  STOP_CACHE,
  stoppedResponse
} from './loader_harness.mjs';

const NOW = 1_000_000;
const CAUSE = 'runtime initialize() failed: Error: migration 0007 failed';

/// The automatic recovery ran: said why, dropped the worker and the caches,
/// and reloaded past the document cache.
function assertRecovered(shell) {
  assert.equal(shell.status.textContent, `The app's runtime stopped: ${CAUSE} — recovering…`);
  assert.equal(shell.session.getItem(RECOVERY_DONE), '1');
  assert.equal(shell.unregistered(), 1);
  assert.deepEqual(shell.cacheNames(), []);
  assert.deepEqual(shell.location.replaced, [`${ORIGIN}/?_freshen=${NOW}`]);
  assert.equal(shell.registered(), 0, 'a recovering load does not register a worker');
}

/// An ordinary boot: nothing recovered, nothing erased.
function assertBootedNormally(shell) {
  assert.equal(shell.registered(), 1);
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.location.replaced, []);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.equal(shell.stuck('impresspress-stopped-cause'), null);
  assert.equal(shell.probes.length, 1);
}

test('a fresh cause left by the worker is shown and recovered from, once', async () => {
  const shell = loadShell({ stop: { reason: CAUSE, at: NOW - 2_000 }, now: NOW });
  await shell.booted;

  assertRecovered(shell);
  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'this build does not erase local data');
});

test('the same recovery erases local data in a build rendered to', async () => {
  const shell = loadShell({ stop: { reason: CAUSE, at: NOW - 2_000 }, now: NOW, wipe: true });
  await shell.booted;

  assertRecovered(shell);
  assert.deepEqual(shell.opfs(), []);
});

// The gate in front of that erasure. Each of these is an entry that cannot be
// shown to be about THIS load; each must leave a wipe-enabled build untouched.
for (const [name, stop] of [
  ['older than a minute', { reason: CAUSE, at: NOW - 60_001 }],
  ['with no timestamp', { reason: CAUSE }],
  ['with a timestamp that is not a number', { reason: CAUSE, at: String(NOW) }],
  ['from the future', { reason: CAUSE, at: NOW + 1 }],
  ['that is not an object', null]
]) {
  test(`a cause ${name} is discarded, not acted on`, async () => {
    const shell = loadShell({ stop, now: NOW, wipe: true });
    await shell.booted;

    assertBootedNormally(shell);
    // Discarded, so it is not there to be misread by the next load either.
    assert.ok(!shell.cacheNames().includes(STOP_CACHE));
  });
}

test('the age window is inclusive at both ends', async () => {
  for (const at of [NOW, NOW - 60_000]) {
    const shell = loadShell({ stop: { reason: CAUSE, at }, now: NOW });
    await shell.booted;
    assertRecovered(shell);
  }
});

test('a cause is read once: the entry is gone after the load that took it', async () => {
  // Without a wipe to hide it: the stuck UI deletes nothing else.
  const shell = loadShell({
    stop: { reason: CAUSE, at: NOW },
    now: NOW,
    session: { [RECOVERY_DONE]: '1' }
  });
  await shell.booted;

  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  // And where "Try again" keeps local data, the screen says that instead.
  assert.match(shell.stuck('impresspress-stopped-next').textContent, /try again, which keeps the data/);
  await shell.stuck('impresspress-retry').click();
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
});

test('a second failure before the app has answered stops on the cause and waits', async () => {
  const shell = loadShell({
    stop: { reason: CAUSE, at: NOW },
    now: NOW,
    wipe: true,
    session: { [RECOVERY_DONE]: '1' }
  });
  await shell.booted;

  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    `The app's runtime stopped: ${CAUSE}`
  );
  // In this rendering "Try again" erases local data, and the screen says so.
  assert.match(shell.stuck('impresspress-stopped-next').textContent, /both erase the data/);
  // Nothing happens by itself: no reload, no wipe, no registration.
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.location.reloads, 0);
  assert.equal(shell.unregistered(), 0);
  assert.equal(shell.registered(), 0);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);

  // "Try again" is the recovery, by choice this time.
  await shell.stuck('impresspress-retry').click();
  assert.deepEqual(shell.location.replaced, [`${ORIGIN}/?_freshen=${NOW}`]);
  assert.deepEqual(shell.opfs(), []);
  assert.equal(shell.session.getItem(RECOVERY_DONE), '1', 'a retry that fails stops here again');
});

test('the cause the worker posts to a listening shell is what the next load shows', async () => {
  const first = loadShell({ probe: () => new Promise(() => {}) });
  // Registered before the probe; give boot() the turns to get there.
  await new Promise((resolve) => setImmediate(resolve));
  first.post({ type: 'sw-self-destruct', reason: CAUSE });
  assert.equal(first.session.getItem(BREAKER), CAUSE);

  const next = loadShell({ session: { [BREAKER]: CAUSE }, now: NOW });
  await next.booted;
  assertRecovered(next);
  assert.equal(next.session.getItem(BREAKER), null, 'the breaker is consumed');
});

test('a probe the runtime dies on is not a boot that worked', async () => {
  // sw.js answers the probe 503 and navigates nobody. Reloading as if it had
  // worked would register, probe and die again — forever.
  const shell = loadShell({ probe: stoppedResponse(CAUSE), now: NOW });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), CAUSE);
  assert.deepEqual(shell.location.replaced, [`${ORIGIN}/`], 'it loads the shell again');
  assert.equal(shell.location.reloads, 0);
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.equal(shell.unregistered(), 0, 'nothing is recovered on this load');

  // The load that starts recovers once…
  const second = loadShell({ session: Object.fromEntries(shell.session.map), now: NOW });
  await second.booted;
  assertRecovered(second);

  // …and when the fresh worker's probe dies the same way, the third load
  // stops with the cause instead of going round again.
  const third = loadShell({
    session: Object.fromEntries(second.session.map),
    probe: stoppedResponse(CAUSE),
    now: NOW
  });
  await third.booted;
  assert.equal(third.session.getItem(RECOVERY_DONE), '1', 'a dead probe does not reset the guard');
  const fourth = loadShell({ session: Object.fromEntries(third.session.map), now: NOW });
  await fourth.booted;
  assert.equal(
    fourth.stuck('impresspress-stopped-cause').textContent,
    `The app's runtime stopped: ${CAUSE}`
  );
  assert.deepEqual(fourth.location.replaced, []);
  assert.equal(fourth.location.reloads, 0);
});

test('a 503 that is not the worker’s stopped-runtime answer is an ordinary answer', async () => {
  const shell = loadShell({
    probe: new Response(JSON.stringify({ error: 'Unavailable', message: 'busy' }), { status: 503 })
  });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), null);
  assert.equal(shell.location.reloads, 1, 'the ordinary reload into the app');
});

test('once the app answers a probe, the next failure gets its own recovery', async () => {
  const shell = loadShell({ session: { [RECOVERY_DONE]: '1' } });
  await shell.booted;

  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.equal(shell.location.reloads, 1);
});

test('the load a recovery lands on probes too, before it goes to the boot URL', async () => {
  // `/?_freshen=…` is not the boot URL, so this load redirects rather than
  // reloads. It is also the first load after every recovery — the one whose
  // probe says whether the recovery worked.
  const search = '?_freshen=999';
  const worked = loadShell({ session: { [RECOVERY_DONE]: '1' }, search });
  await worked.booted;
  assert.equal(worked.probes[0].url, `${ORIGIN}/`);
  assert.equal(worked.session.getItem(RECOVERY_DONE), null);
  assert.deepEqual(worked.location.replaced, [`${ORIGIN}/`]);

  const died = loadShell({
    session: { [RECOVERY_DONE]: '1' },
    search,
    probe: stoppedResponse(CAUSE)
  });
  await died.booted;
  // It loads the shell again with the breaker set — the boot URL here is the
  // shell's own, so the breaker is what tells this from going on into the app.
  assert.equal(died.session.getItem(BREAKER), CAUSE);
  assert.equal(died.session.getItem(RECOVERY_DONE), '1');
  assert.deepEqual(died.location.replaced, [`${ORIGIN}/`]);
  assert.equal(died.location.reloads, 0);
});

// A boot URL that does not answer within the probe's 60 s has proved that the
// app is slow — a cold start, a slow device, a long first migration — and
// nothing about the data in this browser. So running out of time restarts the
// worker once and erases NOTHING, in either rendering; and if the app still
// does not answer, the person is shown the choice.
const TOO_SLOW = 'The app did not answer within 60 seconds';
const TOO_SLOW_NEXT =
  "Restarting it didn't help. It may only be slow: a first start, a large update or a slow device can take longer than this. You can try again, which keeps the data stored locally in this browser, or reset, which erases it and reloads the app.";

for (const [branch, search] of [
  ['reload', ''],
  ['redirect', '?utm=1']
]) {
  for (const wipe of [false, true]) {
    const rendering = wipe ? 'wipe-enabled' : 'default';

    test(`a boot probe that times out restarts once and erases nothing (${branch} branch, ${rendering} build)`, async () => {
      const shell = loadShell({ timesOut: true, search, now: NOW, wipe });
      await shell.booted;

      assert.equal(shell.probes.length, 1);
      assert.equal(
        shell.status.textContent,
        `${TOO_SLOW} — restarting it; the data stored locally in this browser is kept…`
      );
      assert.equal(shell.session.getItem(RECOVERY_DONE), '1');
      assert.equal(shell.unregistered(), 1);
      assert.deepEqual(shell.cacheNames(), []);
      assert.deepEqual(shell.opfs(), ['app.sqlite'], 'a timeout never erases local data');
      assert.deepEqual(shell.location.replaced, [`${ORIGIN}/?_freshen=${NOW}`]);
      assert.equal(shell.location.reloads, 0, 'it does not also go on to the boot URL');
    });

    test(`a second timeout stops and lets the person choose (${branch} branch, ${rendering} build)`, async () => {
      const stuck = () =>
        loadShell({ timesOut: true, search, now: NOW, wipe, session: { [RECOVERY_DONE]: '1' } });

      const shell = stuck();
      await shell.booted;
      // Said as what it is: not "the runtime stopped", which nothing reported.
      assert.equal(shell.stuck('impresspress-stopped-cause').textContent, TOO_SLOW);
      assert.equal(shell.stuck('impresspress-stopped-next').textContent, TOO_SLOW_NEXT);
      // Nothing happens by itself.
      assert.deepEqual(shell.location.replaced, []);
      assert.equal(shell.location.reloads, 0);
      assert.equal(shell.unregistered(), 0);
      assert.deepEqual(shell.opfs(), ['app.sqlite']);

      // "Try again" does what the text says: restarts, keeps the data.
      await shell.stuck('impresspress-retry').click();
      assert.deepEqual(shell.location.replaced, [`${ORIGIN}/?_freshen=${NOW}`]);
      assert.equal(shell.unregistered(), 1);
      assert.deepEqual(shell.opfs(), ['app.sqlite']);

      // "Reset" does what the text says: erases it — the person's choice.
      const other = stuck();
      await other.booted;
      await other.stuck('impresspress-reset').click();
      assert.deepEqual(other.opfs(), []);
      assert.deepEqual(other.location.replaced, [`${ORIGIN}/`]);
    });
  }
}

// One automatic recovery per tab until the app answers, whichever failure
// spent it — and what the stuck screen then offers follows the failure it is
// showing, not the one before.
test('after a timeout restart, a failure the worker reports stops on its cause', async () => {
  const shell = loadShell({
    stop: { reason: CAUSE, at: NOW },
    now: NOW,
    wipe: true,
    session: { [RECOVERY_DONE]: '1' }
  });
  await shell.booted;

  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    `The app's runtime stopped: ${CAUSE}`
  );
  assert.match(shell.stuck('impresspress-stopped-next').textContent, /both erase the data/);
  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'nothing is erased until the person chooses');
});

test('after a wiping recovery, a timeout stops and its "Try again" keeps the data', async () => {
  const shell = loadShell({
    timesOut: true,
    now: NOW,
    wipe: true,
    search: '?_freshen=999',
    session: { [RECOVERY_DONE]: '1' }
  });
  await shell.booted;

  assert.equal(shell.stuck('impresspress-stopped-cause').textContent, TOO_SLOW);
  await shell.stuck('impresspress-retry').click();
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
});

test('a worker that reports a failure as the probe times out is what is acted on', async () => {
  // sw.js posted its cause and is navigating this page to the shell itself;
  // the timer firing in the same moment must not start a second navigation.
  const shell = loadShell({
    timesOut: true,
    now: NOW,
    wipe: true,
    onProbe: ({ post }) => post({ type: 'sw-self-destruct', reason: CAUSE })
  });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), CAUSE);
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
});

// The two failures that reach this shell with no cause and are not a timeout.
// Neither is acted on, so neither can erase anything.
test('a probe that threw proves nothing, clears nothing and erases nothing', async () => {
  const shell = loadShell({
    session: { [RECOVERY_DONE]: '1' },
    wipe: true,
    probe: () => {
      throw new TypeError('Failed to fetch');
    }
  });
  await shell.booted;

  assert.equal(shell.session.getItem(RECOVERY_DONE), '1');
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.equal(shell.stuck('impresspress-stopped-cause'), null);
});

test('a worker that cannot be registered is said, and nothing is recovered or erased', async () => {
  const shell = loadShell({ wipe: true, registerFails: new Error('script evaluation failed') });
  await shell.booted;

  assert.equal(shell.status.textContent, 'Error: script evaluation failed');
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.location.reloads, 0);
  assert.equal(shell.probes.length, 0);
});

// sw.js answers a navigation to a dead runtime with the shell AT THE ADDRESS
// THAT WAS ASKED FOR. Once the recovery has dropped the worker, only the
// static host is left to answer the next load, and a host with no fallback
// has nothing at `/b/auth/login`: every way out of such a shell goes to `/`.
test('a shell standing at a runtime path leaves for its own address, not that path', async () => {
  const path = '/b/auth/login';

  const recovering = loadShell({ path, stop: { reason: CAUSE, at: NOW }, now: NOW });
  await recovering.booted;
  assertRecovered(recovering);

  const stuck = () =>
    loadShell({ path, stop: { reason: CAUSE, at: NOW }, now: NOW, session: { [RECOVERY_DONE]: '1' } });
  const retried = stuck();
  await retried.booted;
  await retried.stuck('impresspress-retry').click();
  assert.deepEqual(retried.location.replaced, [`${ORIGIN}/?_freshen=${NOW}`]);
  const reset = stuck();
  await reset.booted;
  await reset.stuck('impresspress-reset').click();
  assert.deepEqual(reset.location.replaced, [`${ORIGIN}/`]);

  // And a probe the runtime dies on, from such a shell.
  const died = loadShell({ path, probe: stoppedResponse(CAUSE), now: NOW });
  await died.booted;
  assert.deepEqual(died.location.replaced, [`${ORIGIN}/`]);
  assert.equal(died.location.reloads, 0);
});

// What a person reads names the app by what the PAGE says it is, not by a
// name rendered into this script: a shell that is copied and retitled (the
// development sandbox's export) must not go on naming where it came from.
test('visible text names the app the page shows, not the build', async () => {
  const stuckLoad = (page) =>
    loadShell({
      stop: { reason: CAUSE, at: NOW },
      now: NOW,
      session: { [RECOVERY_DONE]: '1' },
      ...page
    });

  const shown = stuckLoad({ title: 'Kiln & Co', documentTitle: 'Something else' });
  await shown.booted;
  assert.equal(shown.stuck('impresspress-stopped-title').textContent, "Kiln & Co couldn't start");

  // A boot page with no title element of the shell's: the document's title.
  const overlaid = stuckLoad({ title: null, documentTitle: 'My own page' });
  await overlaid.booted;
  assert.equal(
    overlaid.stuck('impresspress-stopped-title').textContent,
    "My own page couldn't start"
  );

  // And a page that names nothing at all still says something true.
  const bare = stuckLoad({ title: null, documentTitle: '' });
  await bare.booted;
  assert.equal(bare.stuck('impresspress-stopped-title').textContent, "The app couldn't start");

  // The ordinary boot's progress line, the same way.
  const booting = loadShell({ now: NOW, title: 'Kiln & Co' });
  await booting.booted;
  assert.match(booting.status.textContent, /Loading Kiln & Co\.\.\.$/);
});
