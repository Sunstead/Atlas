import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

// Tailwind only generates the classes it finds, so App.css points it at
// @sunstead/ui's source. If npm installs the package somewhere else (nested
// under app/ instead of hoisted to the root, as 0.1.5's lockfile did), the
// build silently drops every class only the components use.
describe('App.css @source', () => {
  it('points at an installed @sunstead/ui', () => {
    const css = path.resolve(__dirname, 'App.css');
    const sources = [...readFileSync(css, 'utf8').matchAll(/@source '([^']+)'/g)].map((m) => m[1]);
    expect(sources.length).toBeGreaterThan(0);
    for (const source of sources) {
      expect(existsSync(path.resolve(path.dirname(css), source)), source).toBe(true);
    }
  });
});
