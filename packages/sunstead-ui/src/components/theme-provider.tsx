import { useCallback, useEffect, useMemo, useState } from 'react';
import { ThemeContext } from '../hooks/use-theme';
import {
  applyTheme,
  normalizeChoice,
  normalizeSlot,
  resolveTheme,
  type Scheme,
  storageKeys,
  SYSTEM,
  type ThemeChoice,
  type ThemePair,
} from '../lib/themes';

const systemScheme = (): Scheme => (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* the choice still applies this session */
  }
}

/**
 * Owns the theme choice. index.html has already painted the stored theme;
 * this keeps <html> in step as the choice or the OS scheme changes.
 *
 * `app` prefixes the localStorage keys (`<app>-theme`), so each app on the
 * same origin keeps its own choice. It must match the app's index.html.
 */
export function ThemeProvider({ app, children }: { app: string; children: React.ReactNode }) {
  const keys = useMemo(() => storageKeys(app), [app]);
  const [choice, setChoice] = useState<ThemeChoice>(() => normalizeChoice(read(keys.theme)));
  const [pair, setPairState] = useState<ThemePair>(() => ({
    light: normalizeSlot('light', read(keys.light)),
    dark: normalizeSlot('dark', read(keys.dark)),
  }));
  const [system, setSystem] = useState<Scheme>(systemScheme);
  const resolved = resolveTheme(choice, pair, system);

  useEffect(() => {
    const mql = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => setSystem(mql.matches ? 'dark' : 'light');
    mql.addEventListener?.('change', onChange);
    return () => mql.removeEventListener?.('change', onChange);
  }, []);

  useEffect(() => applyTheme(resolved), [resolved]);

  const setThemeId = useCallback(
    (id: string) => {
      const next = normalizeChoice(id);
      write(keys.theme, next);
      setChoice(next);
    },
    [keys],
  );

  const setFollowSystem = useCallback(
    (follow: boolean) => {
      // Turning it off keeps whatever is showing, so nothing jumps.
      const next = follow ? SYSTEM : resolved.id;
      write(keys.theme, next);
      setChoice(next);
    },
    [keys, resolved.id],
  );

  const setPair = useCallback(
    (scheme: Scheme, id: string) => {
      const slot = normalizeSlot(scheme, id);
      write(keys[scheme], slot);
      setPairState((p) => (p[scheme] === slot ? p : { ...p, [scheme]: slot }));
    },
    [keys],
  );

  const followSystem = choice === SYSTEM;
  const value = useMemo(
    () => ({
      themeId: followSystem ? resolved.id : choice,
      setThemeId,
      followSystem,
      setFollowSystem,
      pair,
      setPair,
      resolved,
    }),
    [choice, followSystem, resolved, pair, setThemeId, setFollowSystem, setPair],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}
