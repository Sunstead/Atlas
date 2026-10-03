import { useParams } from '@tanstack/react-router';
import { ItemPreview } from '@/components/item-preview';

/** `/item/$conn/$id`: one item on its own page, linkable. */
export function ItemPage() {
  const { conn, id } = useParams({ from: '/item/$conn/$id' });
  const connection = Number(conn);
  if (!Number.isInteger(connection)) return <p className='p-4 text-sm text-muted-foreground'>Not found.</p>;
  return (
    <div className='mx-auto flex h-[calc(100dvh-3.5rem)] w-full max-w-4xl flex-col'>
      <ItemPreview item={{ connection, id }} />
    </div>
  );
}
