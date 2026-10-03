import { useSearch } from '@tanstack/react-router';

export function SearchPage() {
  const { q } = useSearch({ from: '/search' });

  return (
    <div className='mx-auto w-full max-w-2xl px-4 py-6'>
      {q ? (
        <p className='text-sm text-muted-foreground'>
          No sources are connected yet, so there's nothing to search for "{q}".
        </p>
      ) : (
        <p className='text-sm text-muted-foreground'>Type something to search.</p>
      )}
    </div>
  );
}
