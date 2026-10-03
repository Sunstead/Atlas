import { createContext, useContext } from 'react';
import type { Scheme, ThemeDef, ThemePair } from '../lib/themes';

export type ThemeState = {
  /** The theme chosen outright. While following the system, the one showing. */
  themeId: string;
  /** Picks a theme, and stops following the system. */
  setThemeId: (id: string) => void;
  followSystem: boolean;
  setFollowSystem: (follow: boolean) => void;
  /** The themes Follow system uses for light and dark. */
  pair: ThemePair;
  setPair: (scheme: Scheme, id: string) => void;
  /** The theme on screen now. */
  resolved: ThemeDef;
};

export const ThemeContext = createContext<ThemeState | null>(null);

export function useTheme(): ThemeState {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error('useTheme must be used within ThemeProvider');
  return ctx;
}
