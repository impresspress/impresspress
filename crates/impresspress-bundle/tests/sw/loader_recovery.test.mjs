// What the rendered boot loader does with a dead runtime, and with one that
// has not answered.
//
// Five things here guard something that cannot be taken back or that never
// ends: a recovery may ERASE the app's local data (so it must only do that
// for a failure of the runtime's `initialize()`, and one that is about this
// load); it must run automatically at most once per failure (so the cause
// stays on screen instead of looping) and once per death across tabs (so one
// tab does not undo another's); a boot probe the runtime dies on must not be
// mistaken for a boot that worked; and a start that is only slow must not be
// killed by the page waiting for it.
//
// A recovery replaces the worker IN PLACE — it registers the same script
// under a new URL over the live registration — and then boots on the same
// document. Nothing is unregistered and nothing navigates until the app has
// answered a probe.
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
//     for a death another tab recovered from    nothing replaced or erased
//     while an update has installed a newer version, or already put one in
//                                               nothing replaced or erased:
//                                               the update owns it
//     while an update's install fails           restarts once, as above
//     and the replacement cannot be brought in  the stopped screen
//     in a browser with no Web Locks            restarts once, keeps data
//   a cause entry or breaker that is stale, undated or malformed   no recovery
//   the boot probe ran out of time (either branch)       waits; asks; keeps data
//   the worker does not take the page                    asks; keeps data
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
  NEW_RUNTIME,
  OLD_RUNTIME,
  ORIGIN,
  RECOVERED_CACHE,
  RECOVERY_DONE,
  RECOVERY_LOCK,
  STOP_CACHE,
  stoppedResponse
} from './loader_harness.mjs';

const NOW = 1_000_000;
const CAUSE = 'runtime initialize() failed: Error: migration 0007 failed';
const STOPPED = `The app's runtime stopped: ${CAUSE}`;
const ERASING = `${STOPPED} — recovering; the data stored locally in this browser is being erased…`;
const KEEPING = `${STOPPED} — restarting it; the data stored locally in this browser is kept…`;
// The script URL a recovery registers its replacement under.
const REPLACEMENT = `/sw.js?recovery=${NOW}`;
const DEATH = { id: '0f8fad5b-d9cb-469f-a165-70867728950e' };

// The breaker as the loader writes it: what the worker reported, and when
// this page was told. A report with no death id is held as `''`.
const breaker = (cause, stage, { id = '', at = NOW } = {}) =>
  JSON.stringify({ cause, stage: typeof stage === 'string' ? stage : '', id, runtime: '', at });
const left = (stage, at = NOW - 2_000, death = {}) => ({ reason: CAUSE, stage, at, ...death });
const sessionOf = (shell) => Object.fromEntries(shell.session.map);

/// The worker was replaced in place — by the automatic recovery, or by a
/// button: nothing unregistered, every cache dropped but the record of
/// deaths, the replacement registered under a new script URL, and what was
/// done noted. `erased` is whether it took the local data with it.
function assertReplaced(shell, { erased, automatic = true, caches = [] }) {
  if (automatic) assert.ok(shell.statusLines.includes(erased ? ERASING : KEEPING), shell.statusLines);
  assert.deepEqual(shell.written(RECOVERY_DONE).slice(-1), [erased ? 'erased' : 'restarted']);
  assert.equal(shell.unregistered(), 0, 'a dead worker is replaced, never unregistered');
  assert.deepEqual(shell.cacheNames(), caches);
  assert.equal(shell.registeredUrls.filter((url) => url === REPLACEMENT).length, 1);
  assert.deepEqual(shell.opfs(), erased ? [] : ['app.sqlite']);
}

/// …and, the app having answered the probe that follows on the same
/// document, the recovery is over and the page has gone to `to`: its own
/// address (a reload) when `to` is `null`.
function assertEntered(shell, to = null) {
  assert.equal(shell.session.getItem(RECOVERY_DONE), null, 'the app answered: the recovery is over');
  if (to === null) {
    assert.equal(shell.probes[shell.probes.length - 1].url, shell.location.href);
    assert.equal(shell.location.reloads, 1);
    assert.deepEqual(shell.location.replaced, []);
  } else {
    assert.equal(shell.probes[shell.probes.length - 1].url, to);
    assert.deepEqual(shell.location.replaced, [to]);
    assert.equal(shell.location.reloads, 0);
  }
}

/// Nothing was done to the worker, the caches or the data, and the page went
/// nowhere.
function assertUntouched(shell) {
  assert.equal(shell.unregistered(), 0);
  assert.ok(!shell.registeredUrls.includes(REPLACEMENT), 'no replacement was registered');
  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.location.reloads, 0);
}

/// An ordinary boot: nothing recovered, nothing erased.
function assertBootedNormally(shell) {
  assert.deepEqual(shell.registeredUrls, ['/sw.js']);
  assert.equal(shell.unregistered(), 0);
  assert.deepEqual(shell.written(RECOVERY_DONE), []);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.equal(shell.stuck('impresspress-stopped-cause'), null);
  assert.equal(shell.probes.length, 1);
  assert.equal(shell.location.reloads, 1);
}

// ---------------------------------------------------------------------------
// An insecure page
// ---------------------------------------------------------------------------

test('an insecure page says to use https or localhost, and touches nothing', async () => {
  // A browser offers no service worker to a page that is not a secure
  // context, so the shell must say that rather than blame the browser — and
  // must not reach for a worker, a cause or the local data on the way.
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true, secure: false });
  await shell.booted;

  assert.equal(shell.statusLines.length, 1, String(shell.statusLines));
  assert.match(shell.statusLines[0], /only runs over https or on localhost/);
  assert.ok(shell.statusLines[0].includes(`${ORIGIN} is not one`), shell.statusLines[0]);
  assert.equal(shell.registered(), 0);
  assert.deepEqual(shell.events, []);
  assert.deepEqual(shell.probes, []);
  assert.equal(shell.location.reloads, 0);
});

// ---------------------------------------------------------------------------
// The wipe gate
// ---------------------------------------------------------------------------

test('an initialize() failure is recovered from by erasing, in a build rendered to', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true });
  await shell.booted;

  assertReplaced(shell, { erased: true });
  assertEntered(shell);
});

test('the same failure in a default build restarts and keeps the data', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW });
  await shell.booted;

  assertReplaced(shell, { erased: false });
  assertEntered(shell);
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
    assertReplaced(viaEntry, { erased: false });

    const viaBreaker = loadShell({
      session: { [BREAKER]: breaker(CAUSE, stage) },
      now: NOW,
      wipe: true
    });
    await viaBreaker.booted;
    assertReplaced(viaBreaker, { erased: false });

    // The 503 to the boot probe: this load sets the breaker, the next acts.
    const probing = loadShell({ probe: stoppedResponse(CAUSE, stage), now: NOW, wipe: true });
    await probing.booted;
    assert.deepEqual(probing.opfs(), ['app.sqlite']);
    assert.deepEqual(probing.registeredUrls, ['/sw.js']);
    const viaProbe = loadShell({ session: sessionOf(probing), now: NOW, wipe: true });
    await viaProbe.booted;
    assertReplaced(viaProbe, { erased: false });
  });
}

test('the stage is a field, not the wording of the cause', async () => {
  // CAUSE reads "runtime initialize() failed"; the worker says `request`.
  const shell = loadShell({ stop: left('request'), now: NOW, wipe: true });
  await shell.booted;

  assertReplaced(shell, { erased: false });
});

// ---------------------------------------------------------------------------
// The cause entry and the breaker: about this load, read once
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
    assertReplaced(shell, { erased: false });
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
  assertReplaced(next, { erased: true });
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
  assert.deepEqual(shell.written(RECOVERY_DONE), []);
  assert.deepEqual(shell.registeredUrls, ['/sw.js'], 'nothing is recovered on this load');
  assert.deepEqual(shell.opfs(), ['app.sqlite']);

  // The load that reload starts recovers — and when the replacement's probe
  // dies the same way, the recovery stays spent…
  const second = loadShell({
    session: sessionOf(shell),
    probe: stoppedResponse(CAUSE, 'initialize'),
    now: NOW,
    wipe: true
  });
  await second.booted;
  assertReplaced(second, { erased: true });
  assert.equal(second.session.getItem(RECOVERY_DONE), 'erased', 'a dead probe does not reset the guard');
  assert.equal(second.location.reloads, 1);

  // …so the third load stops with the cause instead of going round.
  const third = loadShell({ session: sessionOf(second), now: NOW, wipe: true });
  await third.booted;
  assert.equal(third.stuck('impresspress-stopped-cause').textContent, STOPPED);
  assert.deepEqual(third.location.replaced, []);
  assert.equal(third.location.reloads, 0);
  assert.deepEqual(third.registeredUrls, []);
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
const ERASE_NOT_DONE =
  "The data stored locally in this browser could not be erased, and restarting didn't help.";
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
  { spent: 'erase-failed', stage: 'initialize', wipe: true, tried: ERASE_NOT_DONE, offer: OFFER_ERASES, label: 'Erase local data and try again', erases: true },
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
    assert.deepEqual(shell.registeredUrls, []);

    // The retry is the recovery for this failure, by choice this time — and
    // it goes on to the app's boot URL, not back to this page: one return to
    // it has already failed.
    await shell.stuck('impresspress-retry').click();
    assertReplaced(shell, { erased: erases, automatic: false });
    assertEntered(shell, `${ORIGIN}/`);

    // "Reset" erases whatever the build and the failure, and keeps nothing
    // of this tab's state.
    const other = stuck();
    await other.booted;
    await other.stuck('impresspress-reset').click();
    assert.deepEqual(other.opfs(), []);
    assert.equal(other.unregistered(), 0);
    assert.deepEqual(other.registeredUrls, [REPLACEMENT]);
    assert.deepEqual(other.location.replaced, [`${ORIGIN}/`]);
    assert.equal(other.session.map.size, 0);
  });
}

// ---------------------------------------------------------------------------
// Where a boot goes
// ---------------------------------------------------------------------------

// A shell standing at the shell's own address is the app being opened, and
// goes to the boot URL. A shell standing anywhere else is a page of the app
// that was asked for — a navigation sw.js answered with the shell because
// its runtime is dead — and since the worker is replaced in place, the same
// document can go straight on to that page: the person ends where they were.
test('a recovery on a page of the app goes on to that page', async () => {
  const shell = loadShell({
    path: '/b/auth/login',
    search: '?redirect=%2Fb%2Fadmin',
    stop: left('request'),
    now: NOW
  });
  await shell.booted;

  assertReplaced(shell, { erased: false });
  assert.equal(shell.probes[0].url, `${ORIGIN}/b/auth/login?redirect=%2Fb%2Fadmin`);
  assertEntered(shell);
});

// The boot URL here is the shell's own address, so the app is opened where
// it was opened: what the address carried — a query, a fragment — is the
// app's to read, and a first visit must not lose it.
test('a first visit keeps the query and the fragment it came with', async () => {
  const shell = loadShell({ search: '?invite=abc#welcome', now: NOW });
  await shell.booted;

  assert.equal(shell.probes[0].url, `${ORIGIN}/?invite=abc#welcome`);
  assertEntered(shell);
});

// The shell goes on to the app only while this document is still the one
// the tab is showing. A navigation that began while the app was starting —
// the person followed a link, typed an address, an agent opened `/llms.txt`
// — is where the tab is going: the worker answers it once the app is up, so
// the shell's own reload would only cancel it and put the tab back here.
// (Both live runs on 2026-10-08 landed on `/` that way.)
test('a navigation that began while the app was starting is left to finish', async () => {
  const shell = loadShell({ onProbe: ({ leave }) => leave(), now: NOW });
  await shell.booted;

  assert.equal(shell.probes.length, 1);
  assert.equal(shell.location.reloads, 0);
  assert.deepEqual(shell.location.replaced, []);
  // The app did answer, so the recovery is over all the same.
  assert.equal(shell.session.getItem(RECOVERY_DONE), null);
  assert.match(shell.status.textContent, /If this page stays, reload it\.$/);

  // Back to this page from the back/forward cache: the shell is on screen
  // again, with nothing left to run — so it goes on to the app now.
  shell.comeBack();
  assert.equal(shell.location.reloads, 1);
});

// …and a probe the runtime died on still leaves its breaker for the next
// load, which is the navigation under way: the dead worker answers it with
// this shell, and that shell recovers.
test('a navigation that began as the probe met a dead runtime still carries the breaker', async () => {
  const shell = loadShell({
    onProbe: ({ leave }) => leave(),
    probe: stoppedResponse(CAUSE, 'initialize'),
    now: NOW
  });
  await shell.booted;

  assert.equal(shell.session.getItem(BREAKER), breaker(CAUSE, 'initialize'));
  assert.equal(shell.location.reloads, 0);
  assert.deepEqual(shell.location.replaced, []);
});

// The loop guard for the return itself: if the page the person was on is
// what kills the runtime, going back to it kills the replacement too.
test('a page that traps the replacement as well ends on the stopped screen, not in a loop', async () => {
  const TRAP = 'error handling request: Error: unreachable executed';

  // 1. The navigation to /b/trap died; the shell replaces the worker and
  //    probes /b/trap — and the fresh runtime dies on it.
  const first = loadShell({
    path: '/b/trap',
    stop: { reason: TRAP, stage: 'request', at: NOW },
    probe: stoppedResponse(TRAP, 'request'),
    now: NOW,
    wipe: true
  });
  await first.booted;
  assert.deepEqual(first.registeredUrls, [REPLACEMENT]);
  assert.equal(first.probes[0].url, `${ORIGIN}/b/trap`);
  assert.equal(first.location.reloads, 1);
  assert.equal(first.session.getItem(RECOVERY_DONE), 'restarted', 'the recovery stays spent');
  assert.deepEqual(first.opfs(), ['app.sqlite']);

  // 2. The reload finds the breaker with the recovery spent: it stops.
  const second = loadShell({ path: '/b/trap', session: sessionOf(first), now: NOW, wipe: true });
  await second.booted;
  assert.equal(
    second.stuck('impresspress-stopped-cause').textContent,
    `The app's runtime stopped: ${TRAP}`
  );
  assert.equal(
    second.stuck('impresspress-stopped-next').textContent,
    `${RESTART_FAILED} ${OFFER_KEEPS}`
  );
  assertUntouched(second);

  // 3. And the way out starts the app from its boot URL, not from /b/trap.
  await second.stuck('impresspress-retry').click();
  assert.equal(second.probes[0].url, `${ORIGIN}/`);
  assert.deepEqual(second.location.replaced, [`${ORIGIN}/`]);
  assert.deepEqual(second.opfs(), ['app.sqlite']);
});

// ---------------------------------------------------------------------------
// Being controlled
// ---------------------------------------------------------------------------

// A page the registered worker does not control cannot probe it — its
// requests go to the static host. The worker's `activate` claims the pages
// there are, so that is rare: a shell the host served after the worker had
// activated, a page loaded past the worker, a shell still under a worker
// that has just been replaced. In each the shell asks to be taken and then
// probes, so that every boot — and every recovery — ends in a probe.
for (const controlled of [false, 'dead']) {
  test(`a shell the registered worker does not control asks for control, then probes (${controlled === false ? 'no controller' : 'another worker'})`, async () => {
    const shell = loadShell({ controlled, session: { [RECOVERY_DONE]: 'restarted' } });
    await shell.booted;

    assert.deepEqual(shell.asked, [{ type: 'impresspress-claim' }]);
    assert.equal(shell.probes.length, 1);
    assert.equal(shell.location.reloads, 1);
    assert.equal(shell.session.getItem(RECOVERY_DONE), null, 'the recovery is over');
  });
}

test('a worker that controls the page is not asked', async () => {
  const shell = loadShell();
  await shell.booted;

  assert.deepEqual(shell.asked, []);
});

test('a worker that never takes the page is waited for and asked about, not navigated to blind', async () => {
  const shell = loadShell({
    path: '/b/auth/login',
    controlled: false,
    claims: false,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await shell.booted;

  assert.equal(shell.probes.length, 0);
  assert.deepEqual(shell.location.replaced, []);
  assert.equal(shell.location.reloads, 0);
  assert.equal(
    shell.stuck('impresspress-stopped-title').textContent,
    'Kiln & Co is taking a long time to start'
  );
  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    'The app has not answered for 10 seconds.'
  );
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'restarted', 'nothing answered: nothing is over');

  await shell.stuck('impresspress-wait').click();
  assert.equal(shell.asked.length, 2);
  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    'The app has not answered for 20 seconds.'
  );
});

// ---------------------------------------------------------------------------
// A boot that has not answered
// ---------------------------------------------------------------------------

// A boot URL that does not answer within the probe's 60 s has proved that the
// app is slow — a cold start, a slow device, a long first migration — and
// nothing else. So the shell never erases for it and never replaces the
// worker on its own: that would kill the very start it is waiting for. It
// waits once more by itself, then asks.
const WAITING_NEXT =
  'It may only be slow: a first start, a large update or a slow device can take longer than this. You can keep waiting. Or restart it, which keeps the data stored locally in this browser, or reset, which erases it; both start the app from its first page, and neither helps an app that is only slow. A restart or reset takes effect only once the app has finished what it is doing now, or the browser has given up on it; nothing is erased before then.';

for (const [branch, search, destination] of [
  ['reload', '', null],
  ['reload with a query', '?utm=1', null]
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
      assert.deepEqual(slow.registeredUrls, ['/sw.js'], 'the worker that is starting is left alone');
      assert.deepEqual(slow.cacheNames(), ['assets-v1']);
      assert.deepEqual(slow.opfs(), ['app.sqlite']);
      assert.deepEqual(slow.written(RECOVERY_DONE), []);
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
      assert.deepEqual(shell.written(RECOVERY_DONE), []);

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
      assert.deepEqual(patient.registeredUrls, ['/sw.js']);
      assert.deepEqual(patient.opfs(), ['app.sqlite']);
      if (destination === null) {
        assert.equal(patient.location.reloads, 1);
      } else {
        assert.deepEqual(patient.location.replaced, [destination]);
      }

      // "Restart it" replaces the worker, keeps the data, and starts the
      // app from its boot URL once the replacement answers.
      const restarting = waiting(2);
      await restarting.booted;
      await restarting.stuck('impresspress-restart').click();
      // What the button says meanwhile is what is happening: the replacement
      // waits for the worker it replaces to stop.
      assert.equal(
        restarting.stuck('impresspress-restart').textContent,
        'Waiting for the app to stop…'
      );
      assert.deepEqual(restarting.registeredUrls, ['/sw.js', REPLACEMENT]);
      assert.deepEqual(restarting.written(RECOVERY_DONE), ['restarted']);
      assert.deepEqual(restarting.opfs(), ['app.sqlite']);
      assert.equal(restarting.unregistered(), 0);
      assert.equal(restarting.probes[2].url, `${ORIGIN}/`);
      assert.equal(restarting.session.getItem(RECOVERY_DONE), null);

      // "Reset" erases — the person's choice. The worker it was waiting on
      // may well be running, so the replacement is brought in FIRST and the
      // data erased after: erased under a running worker, it could be
      // written straight back.
      const resetting = waiting(2);
      await resetting.booted;
      await resetting.stuck('impresspress-reset').click();
      assert.equal(
        resetting.stuck('impresspress-reset').textContent,
        'Waiting for the app to stop…'
      );
      assert.deepEqual(resetting.opfs(), []);
      assert.deepEqual(resetting.events, [
        'register /sw.js',
        `register ${REPLACEMENT}`,
        'erase app.sqlite'
      ]);
      assert.equal(resetting.probes[2].url, `${ORIGIN}/`);
    });
  }
}

test('a timeout after a recovery is still only a timeout', async () => {
  // The replacement a wiping recovery brought in is slow to start (it is
  // rebuilding everything): that is not a second failure.
  const shell = loadShell({
    stop: left('initialize'),
    timesOut: true,
    now: NOW,
    wipe: true
  });
  await shell.booted;

  assertReplaced(shell, { erased: true });
  assert.equal(
    shell.stuck('impresspress-stopped-title').textContent,
    'Kiln & Co is taking a long time to start'
  );
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'erased');
  assert.deepEqual(shell.registeredUrls, [REPLACEMENT], 'and it is not replaced again');
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
  assert.deepEqual(shell.written(RECOVERY_DONE), []);
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
  assert.deepEqual(shell.registeredUrls, ['/sw.js']);
  assert.deepEqual(shell.cacheNames(), ['assets-v1']);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.equal(shell.stuck('impresspress-stopped-cause'), null);
});

test('a worker that cannot be registered is said, and nothing is recovered or erased', async () => {
  const shell = loadShell({ wipe: true, registerFails: new Error('script evaluation failed') });
  await shell.booted;

  assert.equal(shell.status.textContent, 'Error: script evaluation failed');
  assert.deepEqual(shell.written(RECOVERY_DONE), []);
  assertUntouched(shell);
  assert.equal(shell.probes.length, 0);
});

// ---------------------------------------------------------------------------
// Several tabs, one death
// ---------------------------------------------------------------------------

// Every tab of the origin shares the worker and the data, and a dead worker
// tells each of them. The tab that recovers does so for all: it works holding
// a lock, and records the death it recovered from; a tab that comes to the
// same death afterwards must not replace the worker that recovery brought
// in, nor erase the data written since.
test('a recovery reads, decides, wipes and registers holding the lock, and records the death', async () => {
  const shell = loadShell({ stop: left('initialize', NOW, DEATH), now: NOW, wipe: true });
  await shell.booted;

  // One request, made before anything was touched…
  assert.deepEqual(shell.lockRequests, [
    { name: RECOVERY_LOCK, registrations: 0, unregistered: 0, opfs: ['app.sqlite'] }
  ]);
  // …and everything was done under it.
  assertReplaced(shell, { erased: true, caches: [RECOVERED_CACHE] });
  assert.deepEqual(shell.recoveryRecord(), [{ id: DEATH.id, at: NOW }]);
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
    // Tab B: a shell at the person's page. Tab A replaced the worker and
    // recorded the death; its replacement is the registered worker.
    const registered = `${ORIGIN}/sw.js?recovery=${NOW - 1_000}`;
    const shell = loadShell({
      ...source,
      path: '/b/auth/login',
      search: '?next=1',
      registeredUrl: registered,
      recovered: [{ id: DEATH.id, at: NOW - 1_000 }],
      now: NOW,
      wipe: true
    });
    await shell.booted;

    assert.deepEqual(shell.registeredUrls, [registered], 'the worker tab A registered is the one used');
    assert.deepEqual(shell.opfs(), ['app.sqlite'], 'the data written since is left alone');
    assert.ok(shell.cacheNames().includes('assets-v1'));
    assert.equal(shell.session.getItem(BREAKER), null, 'the breaker is consumed');
    assert.deepEqual(shell.written(RECOVERY_DONE), [], 'nothing was spent');
    assert.equal(shell.stuck('impresspress-stopped-cause'), null);
    // It boots here like any shell at a page of the app.
    assertEntered(shell);
    assert.deepEqual(shell.recovered(), [DEATH.id]);
  });
}

test('a different death is recovered from, and both are on record', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, DEATH),
    recovered: [{ id: 'another-death', at: NOW - 1_000 }],
    now: NOW,
    wipe: true
  });
  await shell.booted;

  assertReplaced(shell, { erased: true, caches: [RECOVERED_CACHE] });
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
  assert.deepEqual(old.registeredUrls, [REPLACEMENT]);
  assert.deepEqual(old.recoveryRecord(), [{ id: DEATH.id, at: NOW }]);

  const anonymous = loadShell({
    stop: left('initialize', NOW),
    recovered: [{ id: '', at: NOW }],
    now: NOW
  });
  await anonymous.booted;
  assert.deepEqual(anonymous.registeredUrls, [REPLACEMENT]);
});

test('the buttons do not redo a recovery another tab has done', async () => {
  // This tab sat on the stopped screen while another recovered from the
  // same death: the harness's Cache Storage is this one tab's, so the other
  // tab's record is written into it between the screen and the click.
  const shell = loadShell({
    path: '/b/auth/login',
    stop: left('initialize', NOW, DEATH),
    registeredUrl: `${ORIGIN}/sw.js?recovery=77`,
    now: NOW,
    wipe: true,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await shell.booted;
  assert.equal(shell.stuck('impresspress-retry').textContent, 'Erase local data and try again');
  await shell.recordElsewhere(DEATH.id);

  await shell.stuck('impresspress-retry').click();
  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'nothing is erased again');
  assert.deepEqual(shell.registeredUrls, [`${ORIGIN}/sw.js?recovery=77`], 'nothing is replaced again');
  assert.deepEqual(shell.location.replaced, [`${ORIGIN}/`], 'it boots onto what the other tab left');
  assert.equal(shell.lockRequests.length, 2, 'the button took the lock');

  // With no such record the button does the recovery, and records it.
  const alone = loadShell({
    stop: left('initialize', NOW, DEATH),
    now: NOW,
    wipe: true,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await alone.booted;
  await alone.stuck('impresspress-reset').click();
  assert.deepEqual(alone.recovered(), [DEATH.id]);
  // The boot's and the replacement's. The erase that follows holds the
  // erase lock, taken before the replacement was registered.
  assert.equal(alone.lockRequests.length, 2);
  assert.ok(alone.erasedHolding[0].includes('__impresspress_erase'));
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

  assertReplaced(shell, { erased: false, caches: [RECOVERED_CACHE] });
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
// The worker's script URL
// ---------------------------------------------------------------------------

// A dead worker is replaced by registering the same file under a script URL
// the registration has not had: a browser installs that as a new version,
// which takes the dead one's place in every tab. The same URL again would be
// no update at all — the bytes have not changed — and the dead instance
// would stay.
test('a recovery registers its replacement under a new script URL, over the live registration', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    registeredUrl: `${ORIGIN}/sw.js`,
    now: NOW
  });
  await shell.booted;

  assert.deepEqual(shell.registeredUrls, [REPLACEMENT]);
  assert.equal(shell.unregistered(), 0);
});

// "The script URL the origin already has" is its NEWEST version's. While a
// replacement another tab has just registered is still installing, the
// registration's active worker is still the dead one, under the old URL —
// and registering that URL would be a registration over the replacement,
// which discards it and leaves the tab that made it with nothing.
test('a boot that meets a replacement still installing registers that replacement, not the dead worker under it', async () => {
  const replacing = { active: `${ORIGIN}/sw.js`, installing: `${ORIGIN}${REPLACEMENT}` };

  // A tab that was told of the death and finds it recorded…
  const joining = loadShell({
    session: { [BREAKER]: breaker(CAUSE, 'initialize', DEATH) },
    recovered: [{ id: DEATH.id, at: NOW }],
    registeredUrl: replacing,
    now: NOW,
    wipe: true
  });
  await joining.booted;
  assert.deepEqual(joining.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);

  // …a tab that was told nothing…
  const ordinary = loadShell({ registeredUrl: replacing, now: NOW });
  await ordinary.booted;
  assert.deepEqual(ordinary.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);

  // …and a button pressed on a death that has since been recovered from.
  const stuck = loadShell({
    stop: left('initialize', NOW, DEATH),
    registeredUrl: replacing,
    now: NOW,
    session: { [RECOVERY_DONE]: 'restarted' }
  });
  await stuck.booted;
  await stuck.recordElsewhere(DEATH.id);
  await stuck.stuck('impresspress-retry').click();
  assert.deepEqual(stuck.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);

  // A waiting version counts the same way.
  const waiting = loadShell({
    registeredUrl: { active: `${ORIGIN}/sw.js`, waiting: `${ORIGIN}${REPLACEMENT}` },
    now: NOW
  });
  await waiting.booted;
  assert.deepEqual(waiting.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);
});

test('every other boot registers the script URL the origin already has', async () => {
  // A healthy replacement is not replaced by a boot that has no reason to.
  const kept = loadShell({ registeredUrl: `${ORIGIN}${REPLACEMENT}`, now: NOW });
  await kept.booted;
  assert.deepEqual(kept.registeredUrls, [`${ORIGIN}${REPLACEMENT}`]);

  // No registration: the worker's script, plainly.
  const first = loadShell({ now: NOW });
  await first.booted;
  assert.deepEqual(first.registeredUrls, ['/sw.js']);

  // A registration that is not the worker's script is not followed.
  const odd = loadShell({ registeredUrl: `${ORIGIN}/other.js`, now: NOW });
  await odd.booted;
  assert.deepEqual(odd.registeredUrls, ['/sw.js']);
});

// ---------------------------------------------------------------------------
// An erase that did not complete
// ---------------------------------------------------------------------------

test('an erase is recorded as done only once it is', async () => {
  // The database cannot be removed (a worker still holds it open); the other
  // file goes. The replacement then dies the same way.
  const shell = loadShell({
    stop: left('initialize'),
    probe: stoppedResponse(CAUSE, 'initialize'),
    now: NOW,
    wipe: true,
    opfsFiles: ['app.sqlite', 'uploads'],
    eraseFails: ['app.sqlite']
  });
  await shell.booted;

  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'what could be removed was');
  assert.deepEqual(shell.written(RECOVERY_DONE), ['restarted', 'erase-failed']);
  assert.deepEqual(shell.registeredUrls, [REPLACEMENT], 'the restart still happens');

  // The screen that follows says what happened — not that the data was
  // erased.
  const stuck = loadShell({ session: sessionOf(shell), now: NOW, wipe: true });
  await stuck.booted;
  assert.equal(
    stuck.stuck('impresspress-stopped-next').textContent,
    `${ERASE_NOT_DONE} ${OFFER_ERASES}`
  );
});

// The automatic recovery erases under a worker that is DEAD, and before its
// replacement exists, so that the replacement never starts on what is being
// erased. (The buttons' order is the other way round — the waiting screen's
// test above says why.)
// The worker is dead, so nothing can write the data back while it is
// erased; but the replacement is registered first, so that one that cannot
// be registered — a CDN serving a stale worker under the new URL — has
// replaced nothing and erases nothing.
test('the automatic recovery erases once its replacement is registered', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true });
  await shell.booted;

  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'erase app.sqlite']);
});

test('a replacement that cannot be registered erases nothing, even where the failure would erase', async () => {
  const shell = loadShell({
    stop: left('initialize'),
    now: NOW,
    wipe: true,
    registerFails: new TypeError('Failed to register a ServiceWorker: ServiceWorker script evaluation failed')
  });
  await shell.booted;

  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.events, []);
  assert.ok(shell.stuck('impresspress-retry'));
});

test('a button’s erase that does not complete is shown, and the app is not entered as though it had', async () => {
  const failing = () =>
    loadShell({
      path: '/b/auth/login',
      stop: left('initialize', NOW, DEATH),
      now: NOW,
      wipe: true,
      session: { [RECOVERY_DONE]: 'restarted' },
      opfsFiles: ['app.sqlite', 'uploads'],
      eraseFails: ['app.sqlite']
    });

  for (const button of ['impresspress-reset', 'impresspress-retry']) {
    const shell = failing();
    await shell.booted;
    await shell.stuck(button).click();

    assert.equal(
      shell.stuck('impresspress-stopped-title').textContent,
      "Kiln & Co's local data could not be erased"
    );
    assert.equal(
      shell.stuck('impresspress-stopped-cause').textContent,
      'The data stored locally in this browser could not be erased.'
    );
    assert.equal(shell.stuck('impresspress-retry').textContent, 'Erase local data and try again');
    assert.equal(shell.stuck('impresspress-continue').textContent, 'Continue without erasing');
    assert.equal(shell.stuck('impresspress-reset'), null);
    // Not entered, and — on the reset too — the note of what happened kept.
    assert.equal(shell.probes.length, 0);
    assert.deepEqual(shell.location.replaced, []);
    assert.equal(shell.session.getItem(RECOVERY_DONE), 'erase-failed');
    assert.deepEqual(shell.opfs(), ['app.sqlite']);

    // Trying again tries the erase again, on another replacement…
    await shell.stuck('impresspress-retry').click();
    assert.equal(shell.events.filter((event) => event === 'erase app.sqlite').length, 2);
    assert.equal(shell.probes.length, 0);
    // …and going on without it is the person's to choose.
    await shell.stuck('impresspress-continue').click();
    assert.deepEqual(shell.location.replaced, [`${ORIGIN}/`]);
    assert.deepEqual(shell.opfs(), ['app.sqlite']);
  }
});

test('a recovery that erases nothing never says it erased', async () => {
  const shell = loadShell({ stop: left('request'), now: NOW, wipe: true, eraseFails: ['app.sqlite'] });
  await shell.booted;

  assert.deepEqual(shell.written(RECOVERY_DONE), ['restarted']);
});

// The dead worker is still the one in place when its replacement cannot be
// brought in, so the app is not entered: the stopped screen shows the cause,
// with the retry (another new script URL) and the reset.
test('a replacement that cannot be installed ends on the stopped screen, with the cause', async () => {
  const shell = loadShell({ stop: left('request'), now: NOW, installs: false });
  await shell.booted;

  assert.equal(shell.stuck('impresspress-stopped-cause').textContent, STOPPED);
  assert.ok(shell.stuck('impresspress-retry'));
  assert.ok(shell.stuck('impresspress-reset'));
  assert.equal(shell.probes.length, 0);
  assert.equal(shell.session.getItem(RECOVERY_DONE), 'restarted');
});

// A CDN that answers `/sw.js?recovery=T` with the previous deployment's
// worker: its import of a deleted glue file fails and `register()` rejects.
test('a replacement that cannot be registered ends on the stopped screen too', async () => {
  const shell = loadShell({
    stop: left('request'),
    now: NOW,
    registerFails: new TypeError('Failed to register a ServiceWorker: ServiceWorker script evaluation failed')
  });
  await shell.booted;

  assert.equal(shell.stuck('impresspress-stopped-cause').textContent, STOPPED);
  assert.ok(shell.stuck('impresspress-retry'));
  assert.equal(shell.probes.length, 0);
});

// ---------------------------------------------------------------------------
// One owner of each transition: the recovery, or a deployment's update
// ---------------------------------------------------------------------------

// A deployment's update replaces a worker too, and the browser discards an
// installing version when another is registered over it. A recovery that
// registered its replacement while an update was installing raced it — the
// `sw-update.spec.ts` timeout. The update, once it has installed, owns the
// transition: nothing is replaced, and nothing erased (the new version may
// start on the data the dead one could not).
test('a dead worker an update is replacing is left to the update: nothing replaced or erased', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, DEATH),
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'installs',
    now: NOW,
    wipe: true
  });
  await shell.booted;

  assert.ok(shell.updates() >= 1, 'the host was asked for an update');
  assert.deepEqual(shell.registeredUrls, [], 'nothing was registered over the update');
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.written(RECOVERY_DONE), [], 'no recovery was spent');
  assert.deepEqual(shell.cacheNames().sort(), ['assets-v1', RECOVERED_CACHE].sort());
  // What it waited on, in turn: the check, then the new version's install.
  const waits = shell.statusLines.filter((line) => !line.startsWith('The app') && line.endsWith('…'));
  assert.deepEqual(waits.slice(0, 2), ['Checking for a new version…', 'Updating the app…']);
  assert.ok(
    shell.statusLines.includes(
      `${STOPPED} — a new version is replacing it; the data stored locally in this browser is kept…`
    ),
    shell.statusLines
  );
  // Other tabs with the same death join it instead of recovering.
  assert.deepEqual(shell.recovered(), [DEATH.id]);
  // The page is the new version's, and was entered.
  assert.equal(shell.controller().scriptURL, `${ORIGIN}/sw.js`);
  assert.equal(shell.controller().state, 'activated');
  assertEntered(shell);
});

test('…on the probe road too: a 503 to the boot probe ends in the update, not a recovery', async () => {
  // The probe of an ordinary boot meets the dead worker: the breaker is set
  // and the page reloads, which is all the probe road does by itself…
  const probed = loadShell({
    probe: () => stoppedResponse(CAUSE, 'initialize'),
    registeredUrl: `${ORIGIN}/sw.js`,
    now: NOW,
    wipe: true
  });
  await probed.booted;
  assert.equal(probed.location.reloads, 1);
  assert.deepEqual(probed.registeredUrls, [`${ORIGIN}/sw.js`]);

  // …and the load that acts on it finds the update and leaves it to it.
  const next = loadShell({
    session: sessionOf(probed),
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'installs',
    now: NOW,
    wipe: true
  });
  await next.booted;
  assert.deepEqual(next.registeredUrls, []);
  assert.deepEqual(next.opfs(), ['app.sqlite']);
  assertEntered(next);
});

// "Already in place" is decided by VERSION — the runtime the death names
// against the one the active worker answers with — never by which worker
// controls this page: a shell opened fresh (a hard reload, a new tab) within
// the minute a cause stays fresh has no controller at all, and one an
// update's `clients.claim()` reached first is controlled by the new worker.
// Either way the healthy new version must not be replaced, nor the data
// erased under it.
for (const [how, controlled] of [
  ['a shell with no controller', false],
  ['a shell the new version has already claimed', true]
]) {
  test(`an update already in place owns it as well — ${how}`, async () => {
    const shell = loadShell({
      stop: left('initialize', NOW, { ...DEATH, runtime: OLD_RUNTIME }),
      registeredUrl: `${ORIGIN}/sw.js`,
      update: 'active',
      controlled,
      now: NOW,
      wipe: true
    });
    await shell.booted;

    assert.deepEqual(shell.registeredUrls, []);
    assert.deepEqual(shell.opfs(), ['app.sqlite']);
    assert.deepEqual(shell.recovered(), [DEATH.id]);
    assertEntered(shell);
  });
}

// The same question about a death of the version that is still active: it
// IS that worker, and the recovery replaces it. (A recovery's own
// replacement, under `?recovery=`, is the same version too.)
test('a death of the version still active is recovered from', async () => {
  const shell = loadShell({
    stop: left('request', NOW, { ...DEATH, runtime: OLD_RUNTIME }),
    registeredUrl: `${ORIGIN}/sw.js`,
    controlled: false,
    now: NOW
  });
  await shell.booted;

  assert.deepEqual(shell.registeredUrls, [REPLACEMENT]);
  assert.ok(shell.statusLines.includes('Checking for a new version…'), shell.statusLines);
  assert.ok(!shell.statusLines.includes('Updating the app…'), shell.statusLines);
});

// An update counts only once it has INSTALLED. One whose install fails —
// the runtime binary it must keep is not there, the quota is exhausted —
// replaces nothing, and the dead worker must still be replaced: left to an
// update that never lands, the page would have no way out.
test('an update that fails to install leaves the recovery to run', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'fails',
    now: NOW
  });
  await shell.booted;

  assert.deepEqual(shell.events, ['update discarded', `register ${REPLACEMENT}`]);
  assertReplaced(shell, { erased: false, caches: [RECOVERED_CACHE] });
  assertEntered(shell);
});

test('a button pressed while an update is installing leaves it to the update', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    session: { [RECOVERY_DONE]: 'restarted' },
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'installs',
    now: NOW
  });
  await shell.booted;
  assert.ok(shell.stuck('impresspress-retry'), 'the stopped screen is shown');

  await shell.stuck('impresspress-retry').click();

  assert.deepEqual(shell.registeredUrls, []);
  assert.deepEqual(shell.recovered(), [DEATH.id]);
  assert.equal(shell.controller().state, 'activated');
  assert.equal(shell.probes.length, 1);
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

// With a full quota a version still installs — it keeps no runtime and loads
// it from the host (`keepRuntime` in sw.js; `sw_runtime_kept.test.mjs`) —
// so "Reset" keeps its order: the replacement is brought in and activated,
// then the data is erased, which is what frees the space, then the app is
// entered.
test('a reset completes in its order: replacement in, then the erase, then the app', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    session: { [RECOVERY_DONE]: 'restarted' },
    now: NOW
  });
  await shell.booted;

  await shell.stuck('impresspress-reset').click();

  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'erase app.sqlite']);
  assert.deepEqual(shell.opfs(), []);
  assert.equal(shell.probes.length, 1);
  assert.equal(shell.location.reloads, 1);
});

// ---------------------------------------------------------------------------
// The three bounds and the erase lock
// ---------------------------------------------------------------------------

// A deployment rolled back to a build from before versions were asked: its
// worker never answers the question. That is not a hang — the answer is
// waited for briefly (`RUNTIME_ANSWER_MS`) — and a worker that does not name
// the version that died is not that version.
test('an active worker that never says its version is another version than the one that died', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, { ...DEATH, runtime: NEW_RUNTIME }),
    registeredUrl: `${ORIGIN}/sw.js`,
    answersRuntime: false,
    now: NOW,
    wipe: true
  });
  await shell.booted;

  assert.deepEqual(shell.registeredUrls, []);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.recovered(), [DEATH.id]);
  assertEntered(shell);
});

// The erase and the start of a worker that would open the data take turns
// on one Web Lock (`ERASE_LOCK`; sw.js takes it around `initialize()`).
test('every erase holds the erase lock — the automatic one and the reset', async () => {
  const automatic = loadShell({ stop: left('initialize'), now: NOW, wipe: true });
  await automatic.booted;
  assert.ok(automatic.erasedHolding.length > 0);
  for (const locks of automatic.erasedHolding) assert.ok(locks.includes('__impresspress_erase'), locks);

  const reset = loadShell({
    stop: left('request', NOW, DEATH),
    session: { [RECOVERY_DONE]: 'restarted' },
    now: NOW
  });
  await reset.booted;
  await reset.stuck('impresspress-reset').click();
  assert.ok(reset.erasedHolding.length > 0);
  for (const locks of reset.erasedHolding) assert.ok(locks.includes('__impresspress_erase'), locks);
});

// A version can install and then not activate — Chromium has been seen to
// leave one `installed` with nothing in its way. A shell does not wait on it
// silently: after the control wait it says so and offers the choices.
/// Resolve once `shell` shows the element `id` — for a boot that is still
/// waiting (its `booted` does not settle while the version it waits for has
/// not activated).
async function shown(shell, id) {
  while (!shell.stuck(id)) await new Promise((resolve) => setImmediate(resolve));
}

test('a new version that does not activate is waited for, then asked about', async () => {
  const shell = loadShell({ installs: 'stalls', now: NOW });
  await shown(shell, 'impresspress-wait');

  assert.equal(shell.stuck('impresspress-stopped-title').textContent, 'Kiln & Co is taking a long time to start');
  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    'The new version has not started yet, after 10 seconds.'
  );
  assert.ok(shell.stuck('impresspress-restart'));
  assert.ok(shell.stuck('impresspress-reset'));
  assert.equal(shell.probes.length, 0, 'nothing was asked of a version that is not in place');

  shell.stuck('impresspress-wait').click();
  while (shell.stuck('impresspress-stopped-cause').textContent.includes('after 10 seconds')) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(
    shell.stuck('impresspress-stopped-cause').textContent,
    'The new version has not started yet, after 20 seconds.'
  );
});

// The version the not-started screen is about is the one that has not
// activated — installed and waiting, as Chromium has been seen to leave one
// with nothing in its way. "Restart it" and the reset there register a
// replacement over it: one more wait for that same version is no way out.
// (Registering anew is what gives the browser a new install to activate on.)
// Stuck either way: installed and waiting, or still installing.
for (const [button, label] of [
  ['impresspress-restart', 'Restart it'],
  ['impresspress-reset', 'the reset']
]) {
  for (const [installs, slot, state] of [
    ['stalls', 'waiting', 'installed'],
    ['stalls-installing', 'installing', 'installing']
  ]) {
    test(`${label} on the not-started screen registers a replacement over the version that has not started (${state})`, async () => {
      const shell = loadShell({ installs, registeredUrl: `${ORIGIN}/sw.js`, now: NOW });
      await shown(shell, button);
      const stalled = shell.registration()[slot];
      assert.equal(stalled.state, state);

      shell.stuck(button).click();
      // Bounded: a button that only waits again registers nothing, ever.
      for (let turn = 0; turn < 1_000 && shell.registeredUrls.length < 2; turn += 1) {
        await new Promise((resolve) => setImmediate(resolve));
      }

      assert.deepEqual(shell.registeredUrls, [`${ORIGIN}/sw.js`, REPLACEMENT]);
      const replacing = shell.registration()[slot];
      assert.notEqual(replacing, stalled);
      assert.equal(replacing.scriptURL, `${ORIGIN}${REPLACEMENT}`);
    });
  }
}

// The erase comes BEFORE the replacement loads anything: the runtime holds
// the database in memory and writes it back on every flush, so an erase
// after it has loaded would be undone. ERASE_LOCK is taken before the
// replacement is registered (no worker that could load the data exists
// before then) and let go once the erase is done; sw.js waits for it before
// loading.
test('the automatic erase holds the erase lock from before the replacement is registered', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true });
  await shell.booted;

  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'erase app.sqlite']);
  assert.ok(shell.registeredHolding[0].includes('__impresspress_erase'), shell.registeredHolding);
  assert.ok(shell.erasedHolding[0].includes('__impresspress_erase'), shell.erasedHolding);
  assert.deepEqual(shell.heldNow(), [], 'and let go once the erase is done');
});

test('a replacement that cannot be registered erases nothing and lets the lock go', async () => {
  const shell = loadShell({
    stop: left('initialize'),
    now: NOW,
    wipe: true,
    registerFails: new TypeError('Failed to register a ServiceWorker: ServiceWorker script evaluation failed')
  });
  await shell.booted;

  assert.ok(shell.registeredHolding[0].includes('__impresspress_erase'));
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.heldNow(), []);
});

// A button may find the old worker ALIVE: it erases only once its
// replacement has activated (the old worker is then done writing), and
// holds the erase lock from before registering until then, so the
// replacement cannot load the data first.
test('a reset holds the erase lock from before registering until its erase, after activation', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    session: { [RECOVERY_DONE]: 'restarted' },
    now: NOW
  });
  await shell.booted;

  await shell.stuck('impresspress-reset').click();

  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'erase app.sqlite']);
  for (const locks of [shell.registeredHolding[0], shell.erasedHolding[0]]) {
    assert.ok(locks.includes('__impresspress_erase') && locks.includes('__impresspress_reset'), locks);
  }
  assert.deepEqual(shell.heldNow(), []);
  assert.deepEqual(shell.opfs(), []);
});

test('"Restart it" takes no erase lock', async () => {
  const shell = loadShell({ installs: 'stalls', now: NOW });
  await shown(shell, 'impresspress-restart');

  shell.stuck('impresspress-restart').click();
  while (shell.registeredHolding.length < 2) await new Promise((resolve) => setImmediate(resolve));

  assert.ok(shell.registeredHolding.slice(1).every((locks) => !locks.includes('__impresspress_erase')));
});

// A reset under way in another tab owns the transition: an automatic
// recovery that would erase leaves it to that reset — no replacement over
// the reset's, no second erase, and no wait holding the recovery lock.
test('an automatic erase leaves the transition to a reset under way in another tab', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, DEATH),
    now: NOW,
    wipe: true,
    heldElsewhere: ['__impresspress_reset']
  });
  await shell.booted;

  assert.ok(!shell.registeredUrls.includes(REPLACEMENT));
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
});

// A button that ERASES does not leave the transition to a version already
// in place: that version may have loaded the data — into memory, written
// back on its next flush — and the erase after it would be undone. It brings
// in its own replacement, which waits for the erase before it loads.
test('an erasing button registers its own replacement even over a version already in place', async () => {
  const shell = loadShell({
    stop: left('initialize', NOW, { ...DEATH, runtime: OLD_RUNTIME }),
    session: { [RECOVERY_DONE]: 'restarted' },
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'active',
    now: NOW,
    wipe: true
  });
  await shell.booted;
  assert.equal(shell.stuck('impresspress-retry').textContent, 'Erase local data and try again');

  await shell.stuck('impresspress-retry').click();

  assert.deepEqual(shell.registeredUrls, [REPLACEMENT]);
  assert.deepEqual(shell.opfs(), []);
});

// …while one that keeps the data still leaves it to that version.
test('a button that keeps the data leaves the transition to a version already in place', async () => {
  const shell = loadShell({
    stop: left('request', NOW, { ...DEATH, runtime: OLD_RUNTIME }),
    session: { [RECOVERY_DONE]: 'restarted' },
    registeredUrl: `${ORIGIN}/sw.js`,
    update: 'active',
    now: NOW
  });
  await shell.booted;

  await shell.stuck('impresspress-retry').click();

  assert.deepEqual(shell.registeredUrls, []);
  assert.deepEqual(shell.opfs(), ['app.sqlite']);
});

// The choice screen does not stop the wait under it. A reset waiting there
// holds the erase lock its replacement waits for: when the replacement
// activates, the reset erases and enters the app without anyone clicking.
test('a reset whose replacement activates after the choice is shown carries on by itself', async () => {
  const shell = loadShell({
    stop: left('request', NOW, DEATH),
    session: { [RECOVERY_DONE]: 'restarted' },
    installs: 'late',
    now: NOW
  });
  await shell.booted;

  shell.stuck('impresspress-reset').click();
  await shown(shell, 'impresspress-wait');
  while (shell.location.reloads === 0) await new Promise((resolve) => setTimeout(resolve, 5));

  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'activated late', 'erase app.sqlite']);
  assert.deepEqual(shell.opfs(), []);
  assert.deepEqual(shell.heldNow(), []);
});

// A probe answered "the app is being reset" (sw.js's `beingReset`: an erase
// is under way, so the worker loaded nothing) is not the app answering: the
// recovery flag stays, nothing navigates, and the shell waits for the
// erase and for the version it is for, then asks again.
test('a probe told the app is being reset waits and asks again, leaving the recovery flag alone', async () => {
  const resetting = () =>
    new Response(
      JSON.stringify({ error: 'Unavailable', message: 'x', code: 'app_resetting' }),
      { status: 503, headers: { 'Content-Type': 'application/json' } }
    );
  let asked = 0;
  let flagAtSecondProbe = null;
  const shell = loadShell({
    session: { [RECOVERY_DONE]: 'restarted' },
    registeredUrl: `${ORIGIN}/sw.js`,
    now: NOW,
    probe: () => {
      asked += 1;
      if (asked === 1) return resetting();
      flagAtSecondProbe = shell.session.getItem(RECOVERY_DONE);
      return new Response('<html>', { status: 200 });
    }
  });
  await shell.booted;

  assert.equal(shell.probes.length, 2);
  assert.ok(shell.statusLines.includes('The app is being reset…'), shell.statusLines);
  assert.equal(flagAtSecondProbe, 'restarted', 'the flag was left alone by the first answer');
  assert.equal(shell.session.getItem(RECOVERY_DONE), null, 'and cleared by the real one');
  assert.equal(shell.location.reloads, 1);
});

// The automatic recovery erases only once its replacement has ACTIVATED,
// holding the erase lock from before registering until then: the version
// that died can be started again by the browser, unpoisoned, for some tab's
// request, load the data and write it back at its next flush — and
// activation is when the old version is done with events for good.
test('the automatic erase waits for the replacement to activate, holding the locks until then', async () => {
  const shell = loadShell({ stop: left('initialize', NOW, DEATH), now: NOW, wipe: true, installs: 'late' });
  await shown(shell, 'impresspress-wait');
  assert.deepEqual(shell.opfs(), ['app.sqlite'], 'nothing erased before activation');
  assert.ok(shell.heldNow().includes('__impresspress_erase') && shell.heldNow().includes('__impresspress_reset'));
  assert.deepEqual(shell.lockRequests.length, 1, 'the recovery lock was let go meanwhile');

  while (shell.location.reloads === 0) await new Promise((resolve) => setTimeout(resolve, 5));
  assert.deepEqual(shell.events, [`register ${REPLACEMENT}`, 'activated late', 'erase app.sqlite']);
  assert.deepEqual(shell.opfs(), []);
  assert.deepEqual(shell.heldNow(), []);
  assert.deepEqual(shell.written(RECOVERY_DONE).slice(-1), ['erased']);
});

test('an automatic replacement that is discarded erases nothing and lets the locks go', async () => {
  const shell = loadShell({ stop: left('initialize'), now: NOW, wipe: true, installs: false });
  await shell.booted;

  assert.deepEqual(shell.opfs(), ['app.sqlite']);
  assert.deepEqual(shell.heldNow(), []);
  assert.ok(shell.stuck('impresspress-retry'));
});
