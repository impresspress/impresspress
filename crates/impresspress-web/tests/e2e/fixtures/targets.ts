import type { Page } from '@playwright/test';

/**
 * A phone: narrow, and with touch as its pointer. The touch is what matters
 * to target sizes -- tokens.css raises `--target-min` to 44px under
 * `@media (any-pointer: coarse)`, not below a width -- so a spec that checks a
 * phone's targets runs here rather than in a merely narrow window.
 */
export const PHONE = { viewport: { width: 390, height: 844 }, hasTouch: true } as const;

/**
 * The smallest a control may be for this page's pointer: 44px under a finger,
 * WCAG 2.5.8's 24px for a mouse. Decided from the pointer itself, not read
 * back from the token, so a broken token fails the size checks. Sub-pixel
 * layout rounds a 44px box to 43.99…, so compare with `toBeGreaterThan(floor
 * - 0.5)`.
 */
export async function targetFloor(page: Page): Promise<number> {
  return page.evaluate(() => (matchMedia('(any-pointer: coarse)').matches ? 44 : 24));
}
