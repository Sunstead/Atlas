import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import { storageKeys, THEMES } from '@sunstead/ui/themes';

// index.html paints the stored theme before any module loads, from its own
// copy of the registry. These keep that copy honest.
const html = readFileSync(path.resolve(__dirname, '../../index.html'), 'utf8');

function firstPaintThemes(): Map<string, string> {
  const block = html.match(/var themes = \{([^}]*)\}/);
  expect(block, 'index.html has no `var themes = {...}`').not.toBeNull();
  const map = new Map<string, string>();
  for (const m of block![1].matchAll(/'?([\w-]+)'?\s*:\s*'([a-z]+ [a-z]+)'/g)) map.set(m[1], m[2]);
  return map;
}

describe('index.html first paint', () => {
  it('lists every theme with its scheme and style', () => {
    const expected = new Map(THEMES.map((t) => [t.id, `${t.scheme} ${t.style}`]));
    expect(firstPaintThemes()).toEqual(expected);
  });

  it('reads the keys the provider writes', () => {
    const keys = storageKeys('atlas');
    expect(html).toContain(`localStorage.getItem('${keys.theme}')`);
    expect(html).toContain(`'${keys.theme}-' + s`);
    expect(keys.light).toBe(`${keys.theme}-light`);
  });
});
