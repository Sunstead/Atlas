import { Link, useNavigate, useSearch } from '@tanstack/react-router';
import { Highlighted } from '@/components/highlighted';
import { ItemIcon } from '@/components/item-icon';
import { ItemPreview } from '@/components/item-preview';
import { formatWhen } from '@/lib/format';
import { blobUrl, parsePreviewParam, previewParam, useSearchResults } from '@/lib/items';
import { Film, Images } from 'lucide-react';
import { cn } from '@sunstead/ui/utils';
import type { SearchHit } from '@/generated/SearchHit';
import type { SearchResponse } from '@/generated/SearchResponse';

/**
 * `/search?q=&type=&source=&preview=`: results, and the selected one's
 * preview beside them on wide screens or over them on narrow ones. The
 * selection is in the URL too, so it survives reloads and back/forward.
 */
export function SearchPage() {
  const params = useSearch({ from: '/search' });
  const navigate = useNavigate({ from: '/search' });
  const results = useSearchResults(params);
  const selected = parsePreviewParam(params.preview);

  const select = (hit: SearchHit | null) =>
    navigate({ search: (s) => ({ ...s, preview: hit ? previewParam(hit.item) : undefined }), replace: true });

  return (
    <div className='flex min-h-0 flex-1'>
      <div className={cn('mx-auto w-full max-w-2xl min-w-0 px-4 py-4', selected && 'lg:mx-0 lg:w-[38rem] lg:shrink-0')}>
        <Results q={params.q} data={results.data} loading={results.isFetching} error={results.error} selected={selected} onSelect={select} />
      </div>
      {selected && (
        <aside
          className={cn(
            // A sheet over the results on narrow screens; a pane beside them on wide ones.
            'fixed inset-0 top-14 z-10 bg-background',
            'lg:sticky lg:top-14 lg:z-auto lg:h-[calc(100dvh-3.5rem)] lg:flex-1 lg:border-l',
          )}
        >
          <ItemPreview key={`${selected.connection}:${selected.id}`} item={selected} onClose={() => select(null)} />
        </aside>
      )}
    </div>
  );
}

function Results({
  q,
  data,
  loading,
  error,
  selected,
  onSelect,
}: {
  q?: string;
  data?: SearchResponse;
  loading: boolean;
  error: Error | null;
  selected: { connection: number; id: string } | null;
  onSelect: (hit: SearchHit) => void;
}) {
  if (!q) return <p className='text-sm text-muted-foreground'>Type something to search.</p>;
  if (error) return <p className='text-sm text-error'>Search failed. {error.message}</p>;
  if (!data) return <p className='text-sm text-muted-foreground'>{loading ? 'Searching' : ''}</p>;

  if (data.sources.length === 0) {
    return (
      <p className='text-sm text-muted-foreground'>
        Nothing is connected yet.{' '}
        <Link to='/settings/connections' className='underline'>
          Connect a source
        </Link>{' '}
        to start searching.
      </p>
    );
  }

  const trouble = data.sources.filter((s) => s.state !== 'ok');
  const isActive = (hit: SearchHit) => selected?.connection === hit.item.connection && selected.id === hit.item.id;
  // Photos, videos and albums with a picture show as a strip of thumbnails;
  // everything else as a list.
  const media = data.hits.filter((h) => h.thumbnail && MEDIA.includes(h.kind));
  const rest = data.hits.filter((h) => !media.includes(h));
  return (
    <div className={cn('flex flex-col gap-1 transition-opacity', loading && 'opacity-60')}>
      {trouble.map((s) => (
        <p key={s.connection} className='text-xs text-warning'>
          {s.label}: {s.state === 'timeout' ? "didn't answer in time" : s.message ?? 'failed'}. Its results are missing.
        </p>
      ))}
      {data.hits.length === 0 && <p className='text-sm text-muted-foreground'>Nothing matches "{q}".</p>}
      {media.length > 0 && <MediaStrip hits={media} isActive={isActive} onSelect={onSelect} />}
      {rest.length > 0 && (
        <ul className='flex flex-col'>
          {rest.map((hit) => {
            const active = isActive(hit);
            return (
              <li key={`${hit.item.connection}:${hit.item.id}`}>
                <button
                  type='button'
                  onClick={() => onSelect(hit)}
                  aria-current={active || undefined}
                  className={cn(
                    'flex w-full gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-muted',
                    active && 'bg-muted',
                  )}
                >
                  <ItemIcon mime={hit.mime} className='mt-0.5 size-4 shrink-0 text-muted-foreground' />
                  <span className='min-w-0 flex-1'>
                    <span className='block truncate text-sm font-medium'>{hit.title}</span>
                    <span className='block truncate text-xs text-muted-foreground'>
                      {[hit.path, formatWhen(hit.modified)].filter(Boolean).join(' · ')}
                    </span>
                    {hit.snippet && (
                      <span className='mt-1 line-clamp-2 block text-xs text-muted-foreground'>
                        <Highlighted snippet={hit.snippet} />
                      </span>
                    )}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

const MEDIA = ['photo', 'video', 'album'];

function MediaStrip({
  hits,
  isActive,
  onSelect,
}: {
  hits: SearchHit[];
  isActive: (hit: SearchHit) => boolean;
  onSelect: (hit: SearchHit) => void;
}) {
  return (
    <section aria-label='Photos' className='mb-2'>
      <ul className='-mx-1 flex snap-x gap-2 overflow-x-auto px-1 pb-2'>
        {hits.map((hit) => (
          <li key={`${hit.item.connection}:${hit.item.id}`} className='shrink-0 snap-start'>
            <button
              type='button'
              onClick={() => onSelect(hit)}
              aria-current={isActive(hit) || undefined}
              title={[hit.title, hit.path].filter(Boolean).join(' · ')}
              className={cn(
                'group relative block size-28 overflow-hidden rounded-lg bg-muted ring-offset-2 ring-offset-background transition',
                isActive(hit) ? 'ring-2 ring-primary' : 'hover:opacity-90',
              )}
            >
              <img
                src={blobUrl(hit.item, { variant: 'thumbnail' })}
                alt={hit.title}
                loading='lazy'
                className='size-full object-cover'
              />
              {hit.kind !== 'photo' && (
                <span className='absolute bottom-1 left-1 flex items-center gap-1 rounded bg-black/60 px-1 py-0.5 text-2xs text-white'>
                  {hit.kind === 'video' ? <Film className='size-3' /> : <Images className='size-3' />}
                  {hit.kind === 'album' ? hit.title : 'Video'}
                </span>
              )}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
