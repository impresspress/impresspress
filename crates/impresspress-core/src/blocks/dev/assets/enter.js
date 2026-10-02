// The `/b/dev/enter` page: sign in as the sandbox's bootstrap admin and open
// the workspace, with nothing for the visitor to type.
//
// Inlined into the page by `blocks/dev/enter.rs`, after the auth forms' own
// helpers (`auth_ui/assets/api_post.js`): `apiPost`, which every auth form
// posts through, and `keepSession` / `hasKeptSession`, the one writer and the
// one reader of the session cookie. A classic script, wrapped in its own IIFE so nothing reaches
// `window`.
//
// The session comes from the auth block's own login endpoint — the same
// request the login form makes, through the same function, with the
// credentials the page was rendered with (`data-email` / `data-password`,
// read server-side from the bootstrap config the auth block itself reads).
// Nothing here mints, or is handed, a token any other way.
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
    return apiPost(root.getAttribute('data-login'), {
      email: root.getAttribute('data-email'),
      password: root.getAttribute('data-password')
    }).then(
      function (answer) {
        if (!answer.access_token) {
          giveUp('Could not sign in automatically: the app answered without a session.');
          return;
        }
        // Service-worker synthetic responses do not persist `Set-Cookie`, so
        // the session is kept from the answer — by the login page's own
        // function, so the two cannot write different cookies.
        keepSession(answer);
        openWorkspace();
      },
      function (error) {
        if (error.refused && error.status === 401) {
          // The seeded password no longer opens the account: someone changed
          // it in this instance, which is theirs to do.
          giveUp(
            'One-click entry is off for this sandbox: the admin password was changed here. ' +
              'Sign in with the current password instead.'
          );
          return;
        }
        // Everything else, in `apiPost`'s own words — the same sentence the
        // login form would show: a rate limit's message, a runtime that
        // stopped and why, a request that never reached the app.
        giveUp('Could not sign in automatically: ' + error.message);
      }
    );
  }

  // A visitor who already has an admin session keeps it: signing in again
  // would mint a second session for nothing — and could not, once the owner
  // has changed the password, although the session they are holding is
  // perfectly good. The probe is an endpoint only an admin is answered by;
  // anything but a 200 (an expired session, one without the role, a request
  // that failed) means sign in — and if the app is not answering at all, the
  // sign-in is what says so.
  //
  // Asked only when there is a kept session to ask about. A first-time
  // visitor has none, and probing anyway would put a refused request — a 401
  // in the console, as an error — on the page every newcomer and every agent
  // reads first, for a question whose answer was already known.
  function alreadySignedIn() {
    if (!hasKeptSession()) {
      return Promise.resolve(false);
    }
    return fetch(root.getAttribute('data-session-probe'), { credentials: 'same-origin' }).then(
      function (response) {
        return response.status === 200;
      },
      function () {
        return false;
      }
    );
  }

  alreadySignedIn()
    .then(function (signedIn) {
      return signedIn ? openWorkspace() : signIn();
    })
    .catch(function (error) {
      giveUp('Could not sign in automatically: ' + (error && error.message ? error.message : error));
    });
})();
