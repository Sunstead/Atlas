import type { Snippet } from '@/generated/Snippet';

/** A snippet with its matches marked. Offsets are UTF-16, as the server sends them. */
export function Highlighted({ snippet }: { snippet: Snippet }) {
  const parts: React.ReactNode[] = [];
  let at = 0;
  const ranges = [...snippet.highlights].sort((a, b) => a[0] - b[0]);
  ranges.forEach(([start, end], i) => {
    if (start < at) return;
    if (start > at) parts.push(snippet.text.slice(at, start));
    parts.push(
      <mark key={i} className='rounded-sm bg-primary/15 px-0.5 text-foreground'>
        {snippet.text.slice(start, end)}
      </mark>,
    );
    at = end;
  });
  if (at < snippet.text.length) parts.push(snippet.text.slice(at));
  return <>{parts}</>;
}
