import { useTheme } from '@sunstead/ui/use-theme';
import { SYSTEM, techThemes, themesOf } from '@sunstead/ui/themes';

/** A plain theme picker until settings get a proper page. */
export function ThemeSelect() {
  const { themeId, followSystem, setThemeId, setFollowSystem } = useTheme();

  return (
    <select
      aria-label='Theme'
      className='h-8 rounded-lg border border-input bg-transparent px-2 text-sm text-foreground dark:bg-input/30'
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
      <optgroup label='Tech'>
        {techThemes().map((t) => (
          <option key={t.id} value={t.id}>
            {t.name}
          </option>
        ))}
      </optgroup>
    </select>
  );
}
