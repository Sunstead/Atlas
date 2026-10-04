import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Download, ExternalLink, X } from 'lucide-react';
import { Button, buttonVariants } from '@sunstead/ui/components/button';
import { ItemIcon } from '@/components/item-icon';
import { formatBytes, formatWhen } from '@/lib/format';
import { blobUrl, useItem, usePreview } from '@/lib/items';
import { linkProps } from '@/lib/links';
import type { ItemRef } from '@/generated/ItemRef';
import type { PreviewInfo } from '@/generated/PreviewInfo';

type Ref = Pick<ItemRef, 'connection' | 'id'>;

/** An item's details and its content, shown inside Atlas. */
export function ItemPreview({ item: ref, onClose }: { item: Ref; onClose?: () => void }) {
  const item = useItem(ref);
  const preview = usePreview(ref);

  if (item.isError) {
    return (
      <div className='p-4 text-sm text-muted-foreground'>
        {item.error.message === 'Not found' ? 'This item is gone. It may have been moved or deleted.' : item.error.message}
      </div>
    );
  }
  if (!item.data) return <div className='p-4 text-sm text-muted-foreground'>Loading</div>;
  const info = item.data;
  const meta = [info.path, formatBytes(info.size), formatWhen(info.modified)].filter(Boolean).join(' · ');

  return (
    <div className='flex h-full min-h-0 flex-col'>
      <div className='flex items-start gap-3 border-b p-4'>
        <ItemIcon mime={info.mime} className='mt-0.5 size-5 shrink-0 text-muted-foreground' />
        <div className='min-w-0 flex-1'>
          <h2 className='truncate font-heading font-semibold' title={info.title}>
            {info.title}
          </h2>
          <p className='truncate text-xs text-muted-foreground' title={meta}>
            {info.source_label}
            {meta && ` · ${meta}`}
          </p>
        </div>
        <div className='flex shrink-0 gap-1'>
          {info.url && (
            <a {...linkProps(info.url)} className={buttonVariants({ variant: 'outline', size: 'sm' })}>
              <ExternalLink />
              Open
            </a>
          )}
          <a
            href={blobUrl(ref, { download: true })}
            aria-label='Download'
            title='Download'
            className={buttonVariants({ variant: 'ghost', size: 'icon-sm' })}
          >
            <Download />
          </a>
          {onClose && (
            <Button variant='ghost' size='icon-sm' aria-label='Close preview' title='Close' onClick={onClose}>
              <X />
            </Button>
          )}
        </div>
      </div>
      <div className='min-h-0 flex-1 overflow-auto'>
        {preview.data ? (
          <PreviewBody item={ref} preview={preview.data} />
        ) : preview.isError ? (
          <p className='p-4 text-sm text-muted-foreground'>No preview. {preview.error.message}</p>
        ) : null}
      </div>
    </div>
  );
}

function Truncated({ truncated }: { truncated: boolean }) {
  return truncated ? (
    <p className='px-4 pb-4 text-xs text-muted-foreground'>Showing the start of the file. Open or download it for the rest.</p>
  ) : null;
}

function PreviewBody({ item, preview }: { item: Ref; preview: PreviewInfo }) {
  switch (preview.type) {
    case 'markdown':
      return (
        <>
          <article className='prose-atlas p-4'>
            {/* Links open elsewhere; images in notes aren't fetched (they'd be relative paths). */}
            <Markdown
              remarkPlugins={[remarkGfm]}
              components={{
                a: ({ href, children }) => (
                  <a href={href} target='_blank' rel='noreferrer'>
                    {children}
                  </a>
                ),
                img: ({ alt }) => <span className='text-muted-foreground'>[{alt || 'image'}]</span>,
              }}
            >
              {preview.text}
            </Markdown>
          </article>
          <Truncated truncated={preview.truncated} />
        </>
      );
    case 'text':
      return (
        <>
          <pre className='p-4 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words'>{preview.text}</pre>
          <Truncated truncated={preview.truncated} />
        </>
      );
    case 'image':
      return (
        <div className='flex justify-center p-4'>
          {/* The preview rendition: a format browsers show, even for HEIC or RAW originals. */}
          <img src={blobUrl(item, { variant: 'preview' })} alt='' className='max-h-[70vh] max-w-full rounded-md object-contain' />
        </div>
      );
    case 'pdf':
      return <iframe src={blobUrl(item)} title='PDF preview' className='h-full min-h-[70vh] w-full border-0' />;
    case 'video':
      return (
        <div className='p-4'>
          <video src={blobUrl(item)} controls className='max-h-[70vh] w-full rounded-md' />
        </div>
      );
    case 'audio':
      return (
        <div className='p-4'>
          <audio src={blobUrl(item)} controls className='w-full' />
        </div>
      );
    case 'none':
      return <p className='p-4 text-sm text-muted-foreground'>No preview for this kind of file. Open it in its app or download it.</p>;
  }
}
