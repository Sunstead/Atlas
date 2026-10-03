import { describe, expect, it } from 'vitest';
import { parseSearchParams } from './search-params';

describe('search params', () => {
  it('keeps known string params', () => {
    expect(parseSearchParams({ q: 'tax 2025', type: 'file', source: 'oc', preview: 'oc:1' })).toEqual({
      q: 'tax 2025',
      type: 'file',
      source: 'oc',
      preview: 'oc:1',
    });
  });

  it('treats a numeric query as text', () => {
    expect(parseSearchParams({ q: 42 }).q).toBe('42');
  });

  it('drops empty and unknown values', () => {
    expect(parseSearchParams({ q: '', type: ['a'], other: 'x' })).toEqual({
      q: undefined,
      type: undefined,
      source: undefined,
      preview: undefined,
    });
  });
});
