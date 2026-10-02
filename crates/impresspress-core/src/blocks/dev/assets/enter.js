// The `/b/dev/enter` page: sign in as the sandbox's bootstrap admin and open
// the workspace, with nothing for the visitor to type.
//
// Inlined into the page by `blocks/dev/enter.rs`. A classic script, wrapped in
// its own IIFE so nothing reaches `window`.
//
// The session comes from the auth block's own login endpoint — the same
// request the login form makes, with the credentials the page was rendered
// with (`data-email` / `data-password`, read server-side from the bootstrap
// config the auth block itself reads). Nothing here mints, or is handed, a
// token any other way.
(function () {
  'use strict';

  var root = document.getElementById('dev-enter');
  var status = document.getElementById('dev-enter-status');
  var fallback = document.getElementById('dev-enter-fallback');

  // The normal login page is the fallback for every way this can fail; the
  // sentence says which way it was.
  function giveUp(message) {
    status.textContent = message;
    fallback.hidden = false;
  }

  function openWorkspace() {
    // `replace`, so Back from the workspace does not land on a page whose
    // only act is to send the visitor forward again.
    location.replace(root.getAttribute('data-workspace'));
  }

  function signIn() {
    return fetch(root.getAttribute('data-login'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        email: root.getAttribute('data-email'),
        password: root.getAttribute('data-password')
      })
    }).then(function (response) {
      return response.json().then(function (body) {
        if (response.status === 401) {
          // The seeded password no longer opens the account: someone changed
          // it in this instance, which is theirs to do.
          giveUp(
            'One-click entry is off for this sandbox: the admin password was changed here. ' +
              'Sign in with the current password instead.'
          );
          return;
        }
        if (!response.ok || !body.access_token) {
          giveUp('Could not sign in automatically (HTTP ' + response.status + ').');
          return;
        }
        // Service-worker synthetic responses do not persist `Set-Cookie`, so
        // the session cookie is set here from the response body — the same
        // cookie, with the same attributes, as the login page's own script
        // (`auth_ui/pages/mod.rs`, `login_script`).
        var secure = location.protocol === 'https:' ? '; Secure' : '';
        var maxAge = body.expires_in || 1800;
        document.cookie =
          'auth_token=' + body.access_token + '; Path=/; SameSite=Lax; Max-Age=' + maxAge + secure;
        openWorkspace();
      });
    });
  }

  // A visitor who already has an admin session keeps it: signing in again
  // would mint a second session for nothing — and could not, once the owner
  // has changed the password, although the session they are holding is
  // perfectly good. The probe is an endpoint only an admin is answered by;
  // anything but a 200 (no session, an expired one, one without the role, a
  // request that failed) means sign in.
  fetch(root.getAttribute('data-session-probe'), { credentials: 'same-origin' })
    .then(
      function (response) {
        return response.status === 200;
      },
      function () {
        return false;
      }
    )
    .then(function (signedIn) {
      return signedIn ? openWorkspace() : signIn();
    })
    .catch(function () {
      giveUp('Could not sign in automatically.');
    });
})();
