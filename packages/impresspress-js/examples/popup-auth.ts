import { createImpresspressClient } from '../src/client';

// Initialize the Impresspress client
const client = createImpresspressClient({
  url: 'http://localhost:8090', // Your Impresspress backend URL
});

// Only google/github/microsoft are implemented server-side
// (crates/impresspress-core/src/blocks/auth_ui/oauth/spec.rs) — there is no
// "facebook" provider.

// Example 1: Sign in with Google using a popup
async function signInWithGoogle() {
  try {
    const user = await client.auth.signInWithOAuthPopup('google');
    console.log('Signed in successfully:', user);
  } catch (error) {
    console.error('Sign-in failed:', error);
    // Handle popup blocked or other errors
  }
}

// Example 2: Sign in with Microsoft using a popup
async function signInWithMicrosoft() {
  try {
    const user = await client.auth.signInWithOAuthPopup('microsoft');
    console.log('Signed in successfully:', user);
  } catch (error) {
    console.error('Sign-in failed:', error);
  }
}

// Example 3: Cancel an in-flight popup session (e.g. the user navigates away)
async function signInWithGithubCancellable() {
  const controller = new AbortController();
  const signInPromise = client.auth.signInWithOAuthPopup('github', {
    signal: controller.signal,
  });
  // ... later, e.g. on component unmount:
  // controller.abort();
  return signInPromise;
}

// Example 4: Traditional redirect-based OAuth (no popup)
async function signInWithRedirect(provider: 'google' | 'github' | 'microsoft') {
  try {
    const { auth_url } = await client.auth.signInWithOAuth(provider);
    window.location.href = auth_url;
  } catch (error) {
    console.error('Failed to get OAuth URL:', error);
  }
}

// Example usage in a React/Vue/Svelte component
export function LoginButton() {
  return {
    handleGoogleLogin: signInWithGoogle,
    handleMicrosoftLogin: signInWithMicrosoft,
    handleGithubCancellableLogin: signInWithGithubCancellable,
    handleRedirectLogin: signInWithRedirect,
  };
}
