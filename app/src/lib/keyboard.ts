import { useEffect, useRef } from 'react';

/** Whether a key event happened while typing in a field, where shortcuts mustn't fire. */
export function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  if (target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) return true;
  if (target instanceof HTMLInputElement) {
    return !['checkbox', 'radio', 'button', 'submit', 'reset'].includes(target.type);
  }
  return false;
}

/** The search field, wherever it is on the page. */
export function focusSearch(): boolean {
  const input = document.querySelector<HTMLInputElement>('form[role="search"] input[name="q"]');
  if (!input) return false;
  input.focus();
  input.select();
  return true;
}

/** A window keydown listener that always sees the latest handler. */
export function useKeydown(handler: (e: KeyboardEvent) => void) {
  const ref = useRef(handler);
  useEffect(() => {
    ref.current = handler;
  });
  useEffect(() => {
    const listener = (e: KeyboardEvent) => ref.current(e);
    window.addEventListener('keydown', listener);
    return () => window.removeEventListener('keydown', listener);
  }, []);
}
