import { useTheme } from '@sunstead/ui/use-theme';
import { SYSTEM, themesOf } from '@sunstead/ui/themes';

/** A plain theme picker until settings get a proper page. Tech themes sit under their scheme. */
export function ThemeSelect() {
  const { themeId, followSystem, setThemeId, setFollowSystem } = useTheme();

  return (
    <select
      aria-label='Theme'
      className='h-9 rounded-md border border-input bg-transparent px-2 text-sm text-foreground shadow-xs dark:bg-input/30'
      value={followSystem ? SYSTEM : themeId}
      onChange={(e) => (e.target.value === SYSTEM ? setFollowSystem(true) : setThemeId(e.target.value))}
    >
      <option value={SYSTEM}>Follow system</option>
      <optgroup label='Dark'>
        {themesOf('dark').map((t) => (
          <option key={t.id} value={t.id}>
            {t.name}
          </option>
        ))}
      </optgroup>
      <optgroup label='Light'>
        {themesOf('light').map((t) => (
          <option key={t.id} value={t.id}>
            {t.name}
          </option>
        ))}
      </optgroup>
    </select>
  );
}
