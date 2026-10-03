import { describe, expect, it } from 'vitest';
import { isTyping } from './keyboard';

describe('isTyping', () => {
  it('is true in text fields only', () => {
    const text = document.createElement('input');
    const box = document.createElement('input');
    box.type = 'checkbox';
    const area = document.createElement('textarea');
    const div = document.createElement('div');
    expect(isTyping(text)).toBe(true);
    expect(isTyping(area)).toBe(true);
    expect(isTyping(box)).toBe(false);
    expect(isTyping(div)).toBe(false);
    expect(isTyping(null)).toBe(false);
  });
});
