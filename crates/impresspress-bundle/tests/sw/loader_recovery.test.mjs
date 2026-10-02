// What the rendered boot loader does with a dead runtime, and with one that
// has not answered.
//
// Four things here guard something that cannot be taken back or that never
// ends: a recovery may ERASE the app's local data (so it must only do that
// for a failure of the runtime's `initialize()`, and one that is about this
// load); it must run automatically at most once per failure (so the cause
// stays on screen instead of looping); a boot probe the runtime dies on must
// not be mistaken for a boot that worked; and a start that is only slow must
// not be killed by the page waiting for it.
//
// Every road into a recovery, and what it does to local data (OPFS) in a
// build rendered with `opfs_wipe_on_recovery` (in a default build nothing
// but "Reset" ever erases):
//
//   reported by the worker — its `sw-self-destruct` message (the breaker),
//   the cause it left in Cache Storage, or its 503 to the boot probe —
//     with stage `initialize`                   recovers once, ERASES
//     with stage `load` or `request`            restarts once, keeps data
//     with no stage, or one not known           restarts once, keeps data
//     for a death another tab recovered from    nothing unregistered or erased
//     in a browser with no Web Locks            restarts once, keeps data
//   a cause entry that is stale, undated or malformed    no recovery
//   the boot probe ran out of time (either branch)       waits; asks; keeps data
//   the boot probe could not be made (it threw)          no recovery
//   the worker could not be registered                   no recovery
//   the retry button on the stopped screen               as the failure shown
//   "Keep waiting" / "Restart it" on the waiting screen  keep data
//   "Reset local data and reload"                        the person's choice: erases
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  BREAKER,
  loadShell,
  NEXT_WORKER,
  ORIGIN,
  RECOVERED_CACHE,
  RECOVERY_DONE,
  RECOVERY_LOCK,
  RESUME,
  STOP_CACHE,
  stoppedResponse
} from './loader_harness.mjs';

const NOW = 1_000_000;
const CAUSE = 'runtime initialize() failed: Error: migration 0007 failed';
const STOPPED = `The app's runtime stopped: ${CAUSE}`;
const FRESHEN = `${ORIGIN}/?_freshen=${NOW}`;

// The breaker as the loader writes it: what the worker reported, and when
// this page was told. A report with no death stamp is held as `''` and `0`.
const breaker = (cause, stage, { id = '', diedAt = 0, at = NOW } = {}) =>
  JSON.stringify({ cause, stage: typeof stage === 'string' ? stage : '', id, diedAt, at });
const left = (stage, at = NOW - 2_000, death = {}) => ({ reason: CAUSE, stage, at, ...death });
const sessionOf = (shell) => Object.fromEntries(shell.session.map);

/// A recovery ran — the automatic one, or a button's: said why (when
/// automatic), dropped the worker and the caches, recorded which kind it was,
/// and loaded the shell's own address past the document cache. `erased` is
/// whether it took the local data with it.
function assertRestarted(shell, { erased, automatic = true, registered = 0, caches = [] }) {
  if (automatic) {
    assert.equal(
      shell.status.textContent,
      erased
        ? `${STOPPED} — recovering; the data stored locally in this browser is being erased…`
        : `${STOPPED} — restarting it; the data stored locally in this browser is kept…`
    );
  }
  assert.equal(shell.session.getItem(RECOVERY_DONE), erased ? 'erased' : 'restarted');
  assert.equal(shell.unregistered(), 1);
  // Every cache is dropped but the record of deaths recovered from, which
  // exists once a death with an id has been.
  assert.deepEqual(shell.cacheNames(), caches);
  assert.deepEqual(shell.location.replaced, [FRESHEN]);
  assert.equal(shell.location.reloads, 0);
  // A load that recovers from a reported failure does so before it
  // registers anything; the waiting screen comes after its registration.
  assert.equal(shell.registered(), registered);
  assert.deepEqual(shell.opfs(), erased ? [] : ['app.sqlite']);
}

/// Nothing was done to the worker, the caches or the data, and the page went
/// nowhere.
function assertUntouched(shell) {
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.location.reloads, 0);
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

// ---------------------------------------------------------------------------
// The wipe gate
// ---------------------------------------------------------------------------

test('an initialize() failure is recovered from by erasing, in a build rendered to', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true });
  await shell.booted;

  assertRestarted(shell, { erased: true });
});

test('the same failure in a default build restarts and keeps the data', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW });
  await shell.booted;

  assertRestarted(shell, { erased: false });
});

// Neither says anything about the stored data: a module that could not be
// fetched is a network blip or a deploy in progress, and a runtime that died
// on a request had started on that data. Nor does a stage this script does
// not know, or none at all (a worker from before stages existed).
for (const [name, stage] of [
  ['the wasm module could not be loaded (`load`)', 'load'],
  ['a started runtime died on a request (`request`)', 'request'],
  ['the worker gave no stage', undefined],
  ['the worker gave a stage this build does not know', 'INITIALIZE'],
  ['the stage is not a string', { initialize: true }]
]) {
  test(`a wipe-enabled build keeps the data when ${name} — on every road`, async () => {
    const viaEntry = loadShell({ stop: left(stage), now: NOW, wipe: true });
    await viaEntry.booted;
    assertRestarted(viaEntry, { erased: false });

    const viaBreaker = loadShell({
      session: { [BREAKER]: breaker(CAUSE, stage) },
      now: NOW,
      wipe: true
    });
    await viaBreaker.booted;
    assertRestarted(viaBreaker, { erased: false });

    // The 503 to the boot probe: this load sets the breaker, the next acts.
    const probing = loadShell({ probe: stoppedResponse(CAUSE, stage), now: NOW, wipe: true });
    await probing.booted;
    assert.equal(probing.unregistered(), 0);
    assert.deepEqual(probing.opfs(), ['app.sqlite']);
    const viaProbe = loadShell({ session: sessionOf(probing), now: NOW, wipe: true });
    await viaProbe.booted;
    assertRestarted(viaProbe, { erased: false });
  });
}

test('the stage is a field, not the wording of the cause', async () => {
  // CAUSE reads "runtime initialize() failed"; the worker says `request`.
  const shell = loadShell({ stop: left('request'), now: NOW, wipe: true });
  await shell.booted;

  assertRestarted(shell, { erased: false });
});

// ---------------------------------------------------------------------------
// The cause entry: about this load, read once
// ---------------------------------------------------------------------------

// Each of these is an entry that cannot be shown to be about THIS load; each
// must leave a wipe-enabled build untouched.
for (const [name, stop] of [
  ['older than a minute', { reason: CAUSE, stage: 'initialize', at: NOW - 60_001 }],
  ['with no timestamp', { reason: CAUSE, stage: 'initialize' }],
  ['with a timestamp that is not a number', { reason: CAUSE, stage: 'initialize', at: String(NOW) }],
  ['from the future', { reason: CAUSE, stage: 'initialize', at: NOW + 1 }],
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
    const shell = loadShell({ stop: left('initialize', at), now: NOW });
    await shell.booted;
    assertRestarted(shell, { erased: false });
  }
});

test('a cause is read once: the entry is gone after the load that took it', async () => {
  // On the stopped screen, which deletes nothing else.
  const shell = loadShell({
    stop: left('initialize', NOW),
    now: NOW,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await shell.booted;

  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
});

// The breaker is this tab's own note, and like the entry it is about the load
// that follows it. One that was never followed by a shell load — the worker's
// navigation failed, the tab sat there — must not be acted on by some shell
// load hours later.
for (const [name, value] of [
  ['that this script did not write', 'not json'],
  ['from before breakers were dated', JSON.stringify({ cause: CAUSE, stage: 'initialize' })],
  ['older than a minute', breaker(CAUSE, 'initialize', { at: NOW - 60_001 })],
  ['from the future', breaker(CAUSE, 'initialize', { at: NOW + 1 })]
]) {
  test(`a breaker ${name} is discarded, not acted on`, async () => {
    const shell = loadShell({ session: { [BREAKER]: value }, now: NOW, wipe: true });
    await shell.booted;

    assertBootedNormally(shell);
    assert.equal(shell.session.getItem(BREAKER), null);
  });
}

// ---------------------------------------------------------------------------
// The worker's other two roads
// ---------------------------------------------------------------------------

test('what the worker posts to a listening shell is what the next load acts on', async () => {
  const first = loadShell({ probe: () => new Promise(() => {}) });
  // Registered before the probe; give boot() the turns to get there.
  await new Promise((resolve) => setImmediate(resolve));
  first.post({ type: 'sw-self-destruct', reason: CAUSE, stage: 'initialize' });
  assert.equal(first.session.getItem(BREAKER), breaker(CAUSE, 'initialize'));

  const next = loadShell({ session: sessionOf(first), now: NOW, wipe: true });
  await next.booted;
  assertRestarted(next, { erased: true });
  assert.equal(next.session.getItem(BREAKER), null, 'the breaker is consumed');
});

test('a probe the runtime dies on is not a boot that worked', async () => {
  // sw.js answers the probe 503 and navigates nobody. Going on as if it had
  // worked would land on the shell, probe and get the same answer — forever.
  const shell = loadShell({ probe: stoppedResponse(CAUSE, 'initialize'), now: NOW, wipe: true });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), breaker(CAUSE, 'initialize'));
  // A reload: the dead worker is still registered and answers it with this
  // shell, whatever the static host has at this address.
  assert.equal(shell.location.reloads, 1);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.equal(shell.unregistered(), 0, 'nothing is recovered on this load');
  assert.deepEqual(shell.opfs(), ['app.sqlite']);

  // The load that reload starts recovers once…
  const second = loadShell({ session: sessionOf(shell), now: NOW, wipe: true });
  await second.booted;
  assertRestarted(second, { erased: true });

  // …and when the fresh worker's probe dies the same way, the third load
  // sets the breaker again and the fourth stops with the cause instead of
  // going round.
  const third = loadShell({
    session: sessionOf(second),
    probe: stoppedResponse(CAUSE, 'initialize'),
    now: NOW,
    wipe: true
  });
  await third.booted;
  assert.equal(third.session.getItem(RECOVERY_DONE), 'erased', 'a dead probe does not reset the guard');
  const fourth = loadShell({ session: sessionOf(third), now: NOW, wipe: true });
  await fourth.booted;
  assert.equal(fourth.stuck('impresspress-stopped-cause').textContent, STOPPED);
  assert.deepEqual(fourth.location.replaced, []);
  assert.equal(fourth.location.reloads, 0);
  assert.equal(fourth.unregistered(), 0);
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
  const shell = loadShell({ session: { [RECOVERY_DONE]: 'restarted' } });
  await shell.booted;

  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.equal(shell.location.reloads, 1);
});

// ---------------------------------------------------------------------------
// The stopped screen: the recovery has been spent
// ---------------------------------------------------------------------------

const RESTART_FAILED = "Restarting it didn't help.";
const ERASE_FAILED =
  "Erasing the data stored locally in this browser and restarting didn't resolve it.";
const OFFER_KEEPS =
  'You can try again, which keeps the data stored locally in this browser, or reset, which erases it. Both start the app from its first page.';
const OFFER_ERASES =
  'You can erase the data stored locally in this browser and try again, or reset, which also clears everything else this browser keeps for the app. Both start the app from its first page.';

// Every combination of what was spent and what the retry would do. The first
// sentence is about what was actually done; the second and the button's label
// are about what the retry costs.
for (const { spent, stage, wipe, tried, offer, label, erases } of [
  { spent: 'restarted', stage: 'request', wipe: false, tried: RESTART_FAILED, offer: OFFER_KEEPS, label: 'Try again', erases: false },
  { spent: 'restarted', stage: 'request', wipe: true, tried: RESTART_FAILED, offer: OFFER_KEEPS, label: 'Try again', erases: false },
  { spent: 'restarted', stage: 'load', wipe: true, tried: RESTART_FAILED, offer: OFFER_KEEPS, label: 'Try again', erases: false },
  { spent: 'restarted', stage: 'initialize', wipe: false, tried: RESTART_FAILED, offer: OFFER_KEEPS, label: 'Try again', erases: false },
  { spent: 'restarted', stage: 'initialize', wipe: true, tried: RESTART_FAILED, offer: OFFER_ERASES, label: 'Erase local data and try again', erases: true },
  { spent: 'erased', stage: 'initialize', wipe: true, tried: ERASE_FAILED, offer: OFFER_ERASES, label: 'Erase local data and try again', erases: true },
  { spent: 'erased', stage: 'request', wipe: true, tried: ERASE_FAILED, offer: OFFER_KEEPS, label: 'Try again', erases: false }
]) {
  const rendering = wipe ? 'wipe-enabled' : 'default';
  test(`a second failure stops on the cause and waits (${stage} after ${spent}, ${rendering} build)`, async () => {
    const stuck = () =>
      loadShell({
        path: '/b/auth/login',
        stop: left(stage, NOW),
        now: NOW,
        wipe,
        session: { [RECOVERY_DONE]: spent }
      });

    const shell = stuck();
    await shell.booted;
    assert.equal(shell.stuck('impresspress-stopped-title').textContent, "Kiln & Co couldn't start");
    assert.equal(shell.stuck('impresspress-stopped-cause').textContent, STOPPED);
    assert.equal(shell.stuck('impresspress-stopped-next').textContent, `${tried} ${offer}`);
    assert.equal(shell.stuck('impresspress-retry').textContent, label);
    assert.equal(shell.stuck('impresspress-reset').textContent, 'Reset local data and reload');
    // Nothing happens by itself: no reload, no wipe, no registration.
    assertUntouched(shell);
    assert.equal(shell.registered(), 0);

    // The retry is the recovery for this failure, by choice this time — and
    // from the app's first page: one return to this page has already failed.
    await shell.stuck('impresspress-retry').click();
    assertRestarted(shell, { erased: erases, automatic: false });
    assert.equal(shell.session.getItem(RESUME), null);

    // "Reset" erases whatever the build and the failure, and keeps nothing
    // of this tab's state.
    const other = stuck();
    await other.booted;
    await other.stuck('impresspress-reset').click();
    assert.deepEqual(other.opfs(), []);
    assert.equal(other.unregistered(), 1);
    assert.deepEqual(other.location.replaced, [`${ORIGIN}/`]);
    // …but the script URL the next load is to register its worker under.
    assert.deepEqual([...other.session.map.keys()], [NEXT_WORKER]);
  });
}

// ---------------------------------------------------------------------------
// Coming back to the page
// ---------------------------------------------------------------------------

// sw.js answers a navigation to a dead runtime with the shell AT THE ADDRESS
// THAT WAS ASKED FOR. The recovery unregisters the worker, after which only
// the static host is left to answer — and a host with no fallback has nothing
// at `/b/auth/login`. So the recovery loads `/`, and the boot that follows
// goes back to where the person was.
test('the automatic recovery leaves for the shell’s own address and remembers the page', async () => {
  const shell = loadShell({
    path: '/b/auth/login',
    search: '?redirect=%2Fb%2Fadmin&_freshen=5',
    stop: left('request'),
    now: NOW
  });
  await shell.booted;

  assertRestarted(shell, { erased: false });
  // Without the `_freshen` an earlier recovery put there.
  assert.equal(shell.session.getItem(RESUME), '/b/auth/login?redirect=%2Fb%2Fadmin');

  // A recovery from the shell's own address has nowhere to come back to.
  const atStart = loadShell({ stop: left('request'), now: NOW, search: '?_freshen=5' });
  await atStart.booted;
  assert.equal(atStart.session.getItem(RESUME), null);
});

test('the boot after a recovery probes the remembered page and goes there', async () => {
  const shell = loadShell({
    search: '?_freshen=999',
    session: { [RECOVERY_DONE]: 'restarted', [RESUME]: '/b/auth/login?redirect=%2Fb%2Fadmin' },
    now: NOW
  });
  await shell.booted;

  const page = `${ORIGIN}/b/auth/login?redirect=%2Fb%2Fadmin`;
  assert.equal(shell.probes.length, 1);
  assert.equal(shell.probes[0].url, page);
  assert.deepEqual(shell.location.replaced, [page]);
  assert.equal(shell.session.getItem(RESUME), null, 'read once');
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
});

test('a remembered page is used once, whatever that boot comes to', async () => {
  // Even a boot that never gets to the probe has consumed it.
  const shell = loadShell({
    session: { [RESUME]: '/b/auth/login' },
    registerFails: new Error('script evaluation failed')
  });
  await shell.booted;

  assert.equal(shell.session.getItem(RESUME), null);
});

// The load a recovery lands on comes from the static host. If the worker was
// registered and had activated before this document existed (another tab got
// there first), its claim did not reach this page — and a page it does not
// control cannot probe it. Going straight to the remembered page would leave
// the recovery flag set for good: every recovery has to end in a probe.
test('a shell the registered worker does not control asks for control, then probes', async () => {
  const page = `${ORIGIN}/b/auth/login`;
  for (const controlled of [false, 'dead']) {
    const shell = loadShell({
      controlled,
      search: '?_freshen=999',
      session: { [RESUME]: '/b/auth/login', [RECOVERY_DONE]: 'restarted' }
    });
    await shell.booted;

    assert.deepEqual(shell.asked, [{ type: 'impresspress-claim' }]);
    assert.equal(shell.probes.length, 1);
    assert.equal(shell.probes[0].url, page);
    assert.deepEqual(shell.location.replaced, [page]);
    assert.equal(shell.location.reloads, 0, 'being claimed does not reload the shell');
    assert.equal(shell.session.getItem(RECOVERY_DONE), null, 'the recovery is over');
  }
});

test('a worker that never takes the page is still given the remembered page to load', async () => {
  // A worker from a build that does not answer the request. The page is the
  // right place to go; what is lost is the probe, so the flag stays.
  const shell = loadShell({
    controlled: false,
    claims: false,
    session: { [RESUME]: '/b/auth/login', [RECOVERY_DONE]: 'restarted' }
  });
  await shell.booted;

  assert.deepEqual(shell.location.replaced, [`${ORIGIN}/b/auth/login`]);
  assert.equal(shell.probes.length, 0);
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'restarted');
});

test('a first visit, with nothing remembered, reloads as it always did', async () => {
  const first = loadShell({ controlled: false });
  await first.booted;

  assert.deepEqual(first.asked, []);
  assert.deepEqual(first.location.replaced, []);
  assert.equal(first.location.reloads, 1);
  assert.equal(first.probes.length, 0);
});

test('a remembered address on another origin is not followed', async () => {
  for (const saved of ['//evil.example/x', 'https://evil.example/x']) {
    const shell = loadShell({ session: { [RESUME]: saved } });
    await shell.booted;

    assert.equal(shell.probes[0].url, `${ORIGIN}/`);
    assert.equal(shell.location.reloads, 1, 'the ordinary boot');
    assert.deepEqual(shell.location.replaced, []);
  }
});

// The loop guard for the return itself: if the page the person was on is
// what kills the runtime, going back to it kills the fresh one too.
test('a remembered page that traps again ends on the stopped screen, not in a loop', async () => {
  const TRAP = 'error handling request: Error: unreachable executed';

  // 1. The navigation to /b/trap died; the shell recovers and remembers it.
  const first = loadShell({
    path: '/b/trap',
    stop: { reason: TRAP, stage: 'request', at: NOW },
    now: NOW,
    wipe: true
  });
  await first.booted;
  assert.equal(first.session.getItem(RESUME), '/b/trap');
  assert.deepEqual(first.opfs(), ['app.sqlite']);

  // 2. The boot after it probes /b/trap; the fresh runtime dies on it.
  const second = loadShell({
    search: '?_freshen=1',
    session: sessionOf(first),
    probe: stoppedResponse(TRAP, 'request'),
    now: NOW,
    wipe: true
  });
  await second.booted;
  assert.equal(second.probes[0].url, `${ORIGIN}/b/trap`);
  assert.deepEqual(second.location.replaced, [], 'it does not go there');
  assert.equal(second.location.reloads, 1);
  assert.equal(second.session.getItem(RESUME), null);
  assert.equal(second.session.getItem(RECOVERY_DONE), 'restarted', 'the recovery stays spent');

  // 3. The reload finds the breaker with the recovery spent: it stops.
  const third = loadShell({
    search: '?_freshen=1',
    session: sessionOf(second),
    now: NOW,
    wipe: true
  });
  await third.booted;
  assert.equal(
    third.stuck('impresspress-stopped-cause').textContent,
    `The app's runtime stopped: ${TRAP}`
  );
  assert.equal(
    third.stuck('impresspress-stopped-next').textContent,
    `${RESTART_FAILED} ${OFFER_KEEPS}`
  );
  assertUntouched(third);

  // 4. And the way out starts the app from its first page, not from /b/trap.
  await third.stuck('impresspress-retry').click();
  assert.deepEqual(third.location.replaced, [FRESHEN]);
  assert.equal(third.session.getItem(RESUME), null);
  assert.deepEqual(third.opfs(), ['app.sqlite']);
});

test('the load a recovery lands on probes too, before it goes to the boot URL', async () => {
  // `/?_freshen=…` is not the boot URL, so this load redirects rather than
  // reloads. It is also the first load after every recovery — the one whose
  // probe says whether the recovery worked.
  const search = '?_freshen=999';
  const worked = loadShell({ session: { [RECOVERY_DONE]: 'restarted' }, search });
  await worked.booted;
  assert.equal(worked.probes[0].url, `${ORIGIN}/`);
  assert.equal(worked.session.getItem(RECOVERY_DONE), null);
  assert.deepEqual(worked.location.replaced, [`${ORIGIN}/`]);

  const died = loadShell({
    session: { [RECOVERY_DONE]: 'restarted' },
    search,
    probe: stoppedResponse(CAUSE, 'initialize')
  });
  await died.booted;
  assert.equal(died.session.getItem(BREAKER), breaker(CAUSE, 'initialize'));
  assert.equal(died.session.getItem(RECOVERY_DONE), 'restarted');
  assert.deepEqual(died.location.replaced, [], 'it does not go on to the boot URL');
  assert.equal(died.location.reloads, 1);
});

// ---------------------------------------------------------------------------
// A boot that has not answered
// ---------------------------------------------------------------------------

// A boot URL that does not answer within the probe's 60 s has proved that the
// app is slow — a cold start, a slow device, a long first migration — and
// nothing else. So the shell never erases for it and never restarts the
// worker on its own: a restart would kill the very start it is waiting for.
// It waits once more by itself, then asks.
const WAITING_NEXT =
  'It may only be slow: a first start, a large update or a slow device can take longer than this. You can keep waiting, or restart it, which starts over and so does not help an app that is only slow; both keep the data stored locally in this browser. Or reset, which erases it. Restarting and resetting start the app from its first page.';

for (const [branch, search, destination] of [
  ['reload', '', null],
  ['redirect', '?utm=1', `${ORIGIN}/`]
]) {
  for (const wipe of [false, true]) {
    const rendering = wipe ? 'wipe-enabled' : 'default';

    test(`a probe that runs out of time is waited for, not recovered from (${branch} branch, ${rendering} build)`, async () => {
      // One timeout, then the app answers: the boot simply completes.
      const slow = loadShell({ timesOut: 1, search, now: NOW, wipe });
      await slow.booted;
      assert.equal(slow.probes.length, 2);
      assert.equal(slow.probes[1].url, slow.probes[0].url);
      assert.equal(
        slow.status.textContent,
        'The app has not answered for 60 seconds. Still waiting — it may only be slow…'
      );
      assert.equal(slow.stuck('impresspress-stopped-cause'), null);
      assert.equal(slow.unregistered(), 0, 'the worker that is starting is left alone');
      assert.deepEqual(slow.cacheNames(), ['assets-v1']);
      assert.deepEqual(slow.opfs(), ['app.sqlite']);
      assert.equal(slow.session.getItem(RECOVERY_DONE), null);
      if (destination === null) {
        assert.equal(slow.location.reloads, 1);
        assert.deepEqual(slow.location.replaced, []);
      } else {
        assert.deepEqual(slow.location.replaced, [destination]);
      }
    });

    test(`a second timeout asks, and every choice does what it says (${branch} branch, ${rendering} build)`, async () => {
      const waiting = (timesOut) => loadShell({ timesOut, search, now: NOW, wipe });

      const shell = waiting(true);
      await shell.booted;
      assert.equal(shell.probes.length, 2);
      // Said as what it is: not "the runtime stopped", which nothing reported.
      assert.equal(
        shell.stuck('impresspress-stopped-title').textContent,
        'Kiln & Co is taking a long time to start'
      );
      assert.equal(
        shell.stuck('impresspress-stopped-cause').textContent,
        'The app has not answered for 120 seconds.'
      );
      assert.equal(shell.stuck('impresspress-stopped-next').textContent, WAITING_NEXT);
      assert.equal(shell.stuck('impresspress-wait').textContent, 'Keep waiting');
      assert.equal(shell.stuck('impresspress-restart').textContent, 'Restart it');
      assert.equal(shell.stuck('impresspress-reset').textContent, 'Reset local data and reload');
      assert.equal(shell.stuck('impresspress-retry'), null);
      // Nothing happens by itself, and nothing was spent.
      assertUntouched(shell);
      assert.equal(shell.session.getItem(RECOVERY_DONE), null);

      // "Keep waiting" asks the SAME worker again; still nothing, so it says
      // how long it has been and offers the choice again.
      await shell.stuck('impresspress-wait').click();
      assert.equal(shell.probes.length, 3);
      assert.equal(
        shell.stuck('impresspress-stopped-cause').textContent,
        'The app has not answered for 180 seconds.'
      );
      assertUntouched(shell);

      // "Keep waiting" on an app that then answers is how a slow start ends:
      // the boot completes, with the worker it started with.
      const patient = waiting(2);
      await patient.booted;
      await patient.stuck('impresspress-wait').click();
      assert.equal(patient.probes.length, 3);
      assert.equal(patient.unregistered(), 0);
      assert.deepEqual(patient.opfs(), ['app.sqlite']);
      if (destination === null) {
        assert.equal(patient.location.reloads, 1);
      } else {
        assert.deepEqual(patient.location.replaced, [destination]);
      }

      // "Restart it" replaces the worker and keeps the data.
      const restarting = waiting(true);
      await restarting.booted;
      await restarting.stuck('impresspress-restart').click();
      assertRestarted(restarting, { erased: false, automatic: false, registered: 1 });

      // "Reset" erases — the person's choice.
      const resetting = waiting(true);
      await resetting.booted;
      await resetting.stuck('impresspress-reset').click();
      assert.deepEqual(resetting.opfs(), []);
      assert.deepEqual(resetting.location.replaced, [`${ORIGIN}/`]);
    });
  }
}

test('a timeout after a recovery is still only a timeout', async () => {
  // The fresh worker a wiping recovery left is slow to start (it is
  // rebuilding everything): that is not a second failure.
  const shell = loadShell({
    timesOut: true,
    now: NOW,
    wipe: true,
    search: '?_freshen=999',
    session: { [RECOVERY_DONE]: 'erased' }
  });
  await shell.booted;

  assert.equal(
    shell.stuck('impresspress-stopped-title').textContent,
    'Kiln & Co is taking a long time to start'
  );
  assertUntouched(shell);
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'erased');
});

test('a worker that reports a failure as the probe times out is what is acted on', async () => {
  // sw.js posted its cause and is navigating this page itself; the timer
  // firing in the same moment must not start anything of its own.
  const shell = loadShell({
    timesOut: true,
    now: NOW,
    wipe: true,
    onProbe: ({ post }) => post({ type: 'sw-self-destruct', reason: CAUSE, stage: 'initialize' })
  });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), breaker(CAUSE, 'initialize'));
  assert.equal(shell.probes.length, 1, 'it does not probe again either');
  assert.equal(shell.stuck('impresspress-stopped-cause'), null);
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assertUntouched(shell);
});

// The two failures that reach this shell with no cause and are not a timeout.
// Neither is acted on, so neither can erase anything.
test('a probe that threw proves nothing, clears nothing and erases nothing', async () => {
  const shell = loadShell({
    session: { [RECOVERY_DONE]: 'restarted' },
    wipe: true,
    probe: () => {
      throw new TypeError('Failed to fetch');
    }
  });
  await shell.booted;

  assert.equal(shell.session.getItem(RECOVERY_DONE), 'restarted');
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
  assertUntouched(shell);
  assert.equal(shell.probes.length, 0);
});

// ---------------------------------------------------------------------------
// Several tabs, one death
// ---------------------------------------------------------------------------

// Every tab of the origin shares the worker and the data, and a dead worker
// tells each of them. The tab that recovers does so for all: it works holding
// a lock, and records the death it recovered from; a tab that comes to the
// same death afterwards must not unregister the worker that recovery left,
// nor erase the data written since.
const DEATH = { id: '0f8fad5b-d9cb-469f-a165-70867728950e', diedAt: NOW - 5_000 };

test('a recovery reads, decides and wipes holding the lock, and records the death', async () => {
  const shell = loadShell({ stop: left('initialize', NOW, DEATH), now: NOW, wipe: true });
  await shell.booted;

  // One request, made before anything was touched…
  assert.deepEqual(shell.lockRequests, [
    { name: RECOVERY_LOCK, registrations: 0, unregistered: 0, opfs: ['app.sqlite'] }
  ]);
  // …and by the time it was let go, everything was done.
  assert.equal(shell.unregistered(), 1);
  assert.deepEqual(shell.opfs(), []);
  assert.deepEqual(shell.recovered(), [DEATH.id]);
  // The record outlives the recovery's own cache wipe.
  assert.deepEqual(shell.cacheNames(), [RECOVERED_CACHE]);
});

test('an ordinary boot takes the lock too, and finds nothing to do', async () => {
  const shell = loadShell({ now: NOW });
  await shell.booted;

  assert.equal(shell.lockRequests.length, 1);
  assertBootedNormally(shell);
});

for (const [road, source] of [
  ['the breaker', { session: { [BREAKER]: breaker(CAUSE, 'initialize', DEATH) } }],
  ['the cache entry', { stop: left('initialize', NOW, DEATH) }]
]) {
  test(`a tab told of a death another tab recovered from joins it, erasing nothing (${road})`, async () => {
    // Tab B: a shell at the person's page, still controlled by the dead
    // worker that tab A's recovery unregistered. A recorded the death.
    const shell = loadShell({
      ...source,
      path: '/b/auth/login',
      search: '?next=1',
      controlled: 'dead',
      recovered: [{ id: DEATH.id, at: NOW - 1_000 }],
      now: NOW,
      wipe: true
    });
    await shell.booted;

    assert.equal(shell.unregistered(), 0, 'the worker tab A registered is left alone');
    assert.deepEqual(shell.opfs(), ['app.sqlite'], 'the data written since is left alone');
    assert.ok(shell.cacheNames().includes('assets-v1'));
    assert.equal(shell.session.getItem(BREAKER), null, 'the breaker is consumed');
    assert.equal(shell.session.getItem(RECOVERY_DONE), null, 'nothing was spent');
    assert.equal(shell.stuck('impresspress-stopped-cause'), null);
    // It boots here: joins the registered worker, has it take this page,
    // probes its own address and loads it.
    assert.equal(shell.registered(), 1);
    assert.deepEqual(shell.asked, [{ type: 'impresspress-claim' }]);
    const here = `${ORIGIN}/b/auth/login?next=1`;
    assert.equal(shell.probes[0].url, here);
    assert.equal(shell.location.reloads, 1);
    assert.deepEqual(shell.location.replaced, []);
    assert.deepEqual(shell.recovered(), [DEATH.id]);
  });
}

// A browser answers a registration of the script URL a dead worker was
// registered under — while another tab is still controlled by it — by handing
// that same dead instance back. So a recovery's replacement is registered
// under a URL of its own, and everyone who registers because of that recovery
// uses it.
const REPLACEMENT = `/sw.js?recovery=${NOW}`;

test('a recovery chooses a new script URL for the replacement, and the next load registers it', async () => {
  const shell = loadShell({ stop: left('initialize', NOW, DEATH), now: NOW });
  await shell.booted;
  assert.equal(shell.session.getItem(NEXT_WORKER), REPLACEMENT);
  assert.deepEqual(shell.recoveryRecord(), [{ id: DEATH.id, at: NOW, workerUrl: REPLACEMENT }]);

  // The load the recovery lands on. The origin's registration, as the
  // browser still reports it, is the dead worker's.
  const next = loadShell({
    search: '?_freshen=1',
    session: sessionOf(shell),
    registeredUrl: `${ORIGIN}/sw.js`,
    now: NOW + 50
  });
  await next.booted;
  assert.deepEqual(next.registeredUrls, [REPLACEMENT]);
  assert.equal(next.session.getItem(NEXT_WORKER), null, 'read once');
});

test('a tab that joins a recovery registers the replacement that recovery chose', async () => {
  const shell = loadShell({
    session: { [BREAKER]: breaker(CAUSE, 'initialize', DEATH) },
    controlled: 'dead',
    recovered: [{ id: DEATH.id, at: NOW - 1_000, workerUrl: REPLACEMENT }],
    registeredUrl: `${ORIGIN}/sw.js`,
    now: NOW
  });
  await shell.booted;

  assert.deepEqual(shell.registeredUrls, [REPLACEMENT]);
  assert.equal(shell.unregistered(), 0);
});

test('every other boot registers the script URL the origin already has', async () => {
  // A healthy worker registered by an earlier recovery is not replaced by a
  // boot that has no reason to.
  const kept = loadShell({ registeredUrl: `${ORIGIN}${REPLACEMENT}`, now: NOW });
  await kept.booted;
  assert.deepEqual(kept.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);

  // No registration: the worker's script, plainly.
  const first = loadShell({ now: NOW });
  await first.booted;
  assert.deepEqual(first.registeredUrls, ['/sw.js']);

  // A note or a registration that is not the worker's script is not followed.
  for (const odd of [
    { session: { [NEXT_WORKER]: 'https://evil.example/sw.js' } },
    { session: { [NEXT_WORKER]: '/other.js' } },
    { registeredUrl: `${ORIGIN}/other.js` }
  ]) {
    const shell = loadShell({ ...odd, now: NOW });
    await shell.booted;
    assert.deepEqual(shell.registeredUrls, ['/sw.js']);
  }
});

test('a different death is recovered from, and both are on record', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, DEATH),
    recovered: [{ id: 'another-death', at: NOW - 1_000 }],
    now: NOW,
    wipe: true
  });
  await shell.booted;

  assertRestarted(shell, { erased: true, caches: [RECOVERED_CACHE] });
  assert.deepEqual(shell.recovered(), ['another-death', DEATH.id]);
});

test('a death recorded long ago is forgotten, and a cause with no id matches none', async () => {
  const day = 24 * 60 * 60 * 1000;
  const old = loadShell({
    stop: left('initialize', NOW, DEATH),
    recovered: [{ id: DEATH.id, at: NOW - day - 1 }],
    now: NOW
  });
  await old.booted;
  assert.equal(old.unregistered(), 1);
  assert.deepEqual(old.recovered(), [DEATH.id], 'the stale record was dropped, this one written');

  const anonymous = loadShell({
    stop: left('initialize', NOW),
    recovered: [{ id: '', at: NOW }],
    now: NOW
  });
  await anonymous.booted;
  assert.equal(anonymous.unregistered(), 1);
});

test('the buttons do not redo a recovery another tab has done', async () => {
  const stuck = () =>
    loadShell({
      stop: left('initialize', NOW, DEATH),
      now: NOW,
      wipe: true,
      session: { [RECOVERY_DONE]: 'restarted' }
    });

  // While this tab sat on the stopped screen, another recovered from the
  // same death — recorded here between the screen and the click.
  const shell = stuck();
  await shell.booted;
  assert.equal(shell.stuck('impresspress-retry').textContent, 'Erase local data and try again');
  const joined = loadShell({
    stop: left('initialize', NOW, DEATH),
    recovered: [{ id: DEATH.id, at: NOW }],
    now: NOW,
    wipe: true,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await joined.booted;
  // (A load that finds it already recorded never shows the screen at all.)
  assert.equal(joined.stuck('impresspress-retry'), null);
  assert.equal(joined.unregistered(), 0);

  // The retry and the reset each record the death they dealt with, under
  // the lock, so that no other tab deals with it again.
  await shell.stuck('impresspress-retry').click();
  assert.deepEqual(shell.recovered(), [DEATH.id]);
  assert.equal(shell.lockRequests.length, 2);
  const reset = stuck();
  await reset.booted;
  await reset.stuck('impresspress-reset').click();
  assert.deepEqual(reset.recovered(), [DEATH.id]);
  assert.equal(reset.lockRequests.length, 2);
});

// Without Web Locks two tabs cannot be kept from recovering at once, so the
// one thing that cannot be undone is not done automatically.
test('a browser with no Web Locks never erases automatically', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, DEATH),
    now: NOW,
    wipe: true,
    locks: false
  });
  await shell.booted;

  assertRestarted(shell, { erased: false, caches: [RECOVERED_CACHE] });
  assert.deepEqual(shell.lockRequests, []);

  // The person still can, from the stopped screen: the button says it erases.
  const stuck = loadShell({
    stop: left('initialize', NOW, DEATH),
    now: NOW,
    wipe: true,
    locks: false,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await stuck.booted;
  assert.equal(stuck.stuck('impresspress-retry').textContent, 'Erase local data and try again');
  await stuck.stuck('impresspress-retry').click();
  assert.deepEqual(stuck.opfs(), []);
});

// ---------------------------------------------------------------------------
// An erase that did not complete
// ---------------------------------------------------------------------------

test('an erase is recorded as done only once it is', async () => {
  // The database is held open by a worker that is still running; the other
  // file goes.
  const shell = loadShell({
    stop: left('initialize'),
    now: NOW,
    wipe: true,
    opfsFiles: ['app.sqlite', 'uploads'],
    eraseFails: ['app.sqlite']
  });
  await shell.booted;

  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'what could be removed was');
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'erase-failed');
  assert.deepEqual(shell.location.replaced, [FRESHEN], 'the restart still happens');

  // And if the app then fails again, the screen says what happened — not
  // that the data was erased.
  const stuck = loadShell({
    stop: left('initialize', NOW),
    now: NOW,
    wipe: true,
    session: sessionOf(shell)
  });
  await stuck.booted;
  assert.equal(
    stuck.stuck('impresspress-stopped-next').textContent,
    `The data stored locally in this browser could not be erased, and restarting didn't help. ${OFFER_ERASES}`
  );
});

test('a recovery that erases nothing never says it erased', async () => {
  const shell = loadShell({ stop: left('request'), now: NOW, wipe: true, eraseFails: ['app.sqlite'] });
  await shell.booted;

  assert.equal(shell.session.getItem(RECOVERY_DONE), 'restarted');
});

// ---------------------------------------------------------------------------
// Naming the app
// ---------------------------------------------------------------------------

// What a person reads names the app by what the PAGE says it is, not by a
// name rendered into this script: a shell that is copied and retitled (the
// development sandbox's export) must not go on naming where it came from.
test('visible text names the app the page shows, not the build', async () => {
  const stuckLoad = (page) =>
    loadShell({
      stop: left('request', NOW),
      now: NOW,
      session: { [RECOVERY_DONE]: 'restarted' },
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
