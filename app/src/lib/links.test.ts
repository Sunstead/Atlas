import { describe, expect, it } from 'vitest';
import { isWebLink, linkProps } from '@/lib/links';

describe('links', () => {
  it('opens web apps in a new tab and app links in place', () => {
    expect(isWebLink('https://opencloud.example/f/1')).toBe(true);
    expect(isWebLink('HTTP://localhost:1420')).toBe(true);
    expect(isWebLink('solstice://open?vault=Notes&path=a.md')).toBe(false);
    expect(linkProps('https://immich.example/photos/1')).toEqual({
      href: 'https://immich.example/photos/1',
      target: '_blank',
      rel: 'noreferrer',
    });
    expect(linkProps('solstice://open?vault=Notes&path=a.md')).toEqual({ href: 'solstice://open?vault=Notes&path=a.md' });
  });
});
