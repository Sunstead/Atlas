import { useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { Search } from 'lucide-react';
import { Input } from '@sunstead/ui/components/input';
import { cn } from '@sunstead/ui/utils';

/**
 * The search field. Submitting goes to `/search?q=`, the same URL the
 * browser's search engine entry uses. Remount it (via `key`) to reset it
 * to a new query.
 */
export function SearchBox({
  initial = '',
  autoFocus = false,
  className,
  size = 'default',
}: {
  initial?: string;
  autoFocus?: boolean;
  className?: string;
  size?: 'default' | 'lg';
}) {
  const navigate = useNavigate();
  const [q, setQ] = useState(initial);

  return (
    <form
      role='search'
      className={cn('relative w-full', className)}
      onSubmit={(e) => {
        e.preventDefault();
        const query = q.trim();
        if (query) navigate({ to: '/search', search: { q: query } });
      }}
    >
      <Search className='pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground' />
      <Input
        type='search'
        name='q'
        aria-label='Search'
        placeholder='Search files, photos and more'
        autoComplete='off'
        autoFocus={autoFocus}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        className={cn('pl-8', size === 'lg' && 'h-11 text-base md:text-base')}
      />
    </form>
  );
}
