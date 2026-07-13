import { createImpresspressClient } from '../src/client';

// Initialize the Impresspress client
const client = createImpresspressClient({
  url: 'http://localhost:8080', // Your Impresspress backend URL
});

// Example 1: Sign in with Google using popup
async function signInWithGoogle() {
  try {
    const { user, tokens } = await client.auth.signInWithPopup('google');
    console.log('Signed in successfully:', user);
    console.log('Access token:', tokens.access_token);
  } catch (error) {
    console.error('Sign-in failed:', error);
    // Handle popup blocked or other errors
  }
}

// Example 2: Sign in with Microsoft using popup
async function signInWithMicrosoft() {
  try {
    const { user, tokens } = await client.auth.signInWithPopup('microsoft');
    console.log('Signed in successfully:', user);
  } catch (error) {
    console.error('Sign-in failed:', error);
  }
}

// Example 3: Sign in with Facebook using popup
async function signInWithFacebook() {
  try {
    const { user, tokens } = await client.auth.signInWithPopup('facebook');
    console.log('Signed in successfully:', user);
  } catch (error) {
    console.error('Sign-in failed:', error);
  }
}

// Example 4: Traditional redirect-based OAuth (optional fallback)
async function signInWithRedirect(provider: 'google' | 'microsoft' | 'facebook') {
  try {
    const { url } = await client.auth.signInWithOAuth(provider);
    // Redirect the user to the OAuth provider
    window.location.href = url;
  } catch (error) {
    console.error('Failed to get OAuth URL:', error);
  }
}

// Example usage in a React/Vue/Svelte component
export function LoginButton() {
  return {
    handleGoogleLogin: signInWithGoogle,
    handleMicrosoftLogin: signInWithMicrosoft,
    handleFacebookLogin: signInWithFacebook,
    handleRedirectLogin: signInWithRedirect,
  };
}