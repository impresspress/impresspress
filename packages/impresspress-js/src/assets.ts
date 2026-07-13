/**
 * Static assets module for Impresspress SDK
 * Provides paths to bundled static assets like logos and icons
 */

// Asset paths relative to the package root
export const IMPRESSPRESS_ASSETS = {
  logo: '@impresspress/sdk/static/logo.png',
  logoLong: '@impresspress/sdk/static/logo_long.png',
  favicon: '@impresspress/sdk/static/favicon.ico',
} as const;

// Helper to get the absolute path to assets
export function getImpresspressAssetPath(asset: keyof typeof IMPRESSPRESS_ASSETS): string {
  return `/node_modules/${IMPRESSPRESS_ASSETS[asset]}`;
}

// Export for convenience
export const impresspressAssets = IMPRESSPRESS_ASSETS;