import { describe, expect, it } from 'vitest';
import { formatBytes, formatWhen } from './format';
import { parsePreviewParam, previewParam } from './items';

describe('formatBytes', () => {
  it('scales units', () => {
    expect(formatBytes(1)).toBe('1 byte');
    expect(formatBytes(12)).toBe('12 bytes');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(5 * 1024 * 1024)).toBe('5.0 MB');
    expect(formatBytes(300 * 1024 * 1024)).toBe('300 MB');
    expect(formatBytes(null)).toBe('');
  });
});

describe('formatWhen', () => {
  const now = Date.UTC(2026, 9, 2, 12, 0, 0);
  const secs = (ms: number) => Math.floor(ms / 1000);

  it('is relative for recent times', () => {
    expect(formatWhen(secs(now) - 10, now)).toBe('just now');
    expect(formatWhen(secs(now) - 5 * 60, now)).toMatch(/5 minutes ago/);
    expect(formatWhen(secs(now) - 3 * 3600, now)).toMatch(/3 hours ago/);
  });

  it('is a date for older ones', () => {
    expect(formatWhen(secs(Date.UTC(2024, 0, 15)), now)).toMatch(/2024/);
    expect(formatWhen(null, now)).toBe('');
  });
});

describe('preview param', () => {
  it('round trips', () => {
    const ref = { connection: 3, id: 'RG9jdW1lbnRz_-' };
    expect(parsePreviewParam(previewParam(ref))).toEqual(ref);
  });

  it('rejects junk', () => {
    expect(parsePreviewParam(undefined)).toBeNull();
    expect(parsePreviewParam('x:abc')).toBeNull();
    expect(parsePreviewParam('3:has spaces')).toBeNull();
  });
});
