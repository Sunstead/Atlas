/** `1.4 MB`, `820 KB`, `12 bytes`. */
export function formatBytes(n: number | null | undefined): string {
  if (n == null) return '';
  if (n < 1024) return `${n} ${n === 1 ? 'byte' : 'bytes'}`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** Recent times relative (`5 minutes ago`), older ones as a date. `secs` is Unix seconds. */
export function formatWhen(secs: number | null | undefined, now: number = Date.now()): string {
  if (secs == null) return '';
  const diff = Math.round(now / 1000 - secs);
  if (diff < 45) return 'just now';
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  if (diff < 3600) return rtf.format(-Math.round(diff / 60), 'minute');
  if (diff < 86400) return rtf.format(-Math.round(diff / 3600), 'hour');
  if (diff < 7 * 86400) return rtf.format(-Math.round(diff / 86400), 'day');
  const d = new Date(secs * 1000);
  const sameYear = d.getFullYear() === new Date(now).getFullYear();
  return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: sameYear ? undefined : 'numeric' });
}
