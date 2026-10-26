import type { Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

/**
 * The document-structure rules axe holds every page to (PLAN §5.4, S8):
 * `heading-order`, the `landmark-*` rules and `region`. Colour contrast is
 * not run: the Rust value-based guards own it, and axe's pixel heuristic
 * would report the same pairs a second time.
 */
export const STRUCTURE_RULES = [
  'heading-order',
  'region',
  'landmark-banner-is-top-level',
  'landmark-complementary-is-top-level',
  'landmark-contentinfo-is-top-level',
  'landmark-main-is-top-level',
  'landmark-no-duplicate-banner',
  'landmark-no-duplicate-contentinfo',
  'landmark-no-duplicate-main',
  'landmark-one-main',
  'landmark-unique',
];

/** What axe finds on `page` under `STRUCTURE_RULES`, one line per node. */
export async function structureViolations(page: Page): Promise<string[]> {
  const result = await new AxeBuilder({ page }).withRules(STRUCTURE_RULES).analyze();
  return result.violations.flatMap((v) => v.nodes.map((n) => `${v.id}: ${n.target.join(' ')}`));
}
