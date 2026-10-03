import { useQuery } from '@tanstack/react-query';
import { SearchBox } from '@/components/search-box';
import { api } from '@/lib/api';
import type { ServerInfo } from '@/generated/ServerInfo';

export function HomePage() {
  const info = useQuery({ queryKey: ['info'], queryFn: () => api<ServerInfo>('/v1/info') });

  return (
    <div className='flex flex-1 flex-col items-center justify-center gap-6 px-4 pb-[20vh]'>
      <h1 className='font-heading text-3xl font-semibold tracking-tight'>Atlas</h1>
      <SearchBox autoFocus size='lg' className='max-w-2xl' />
      <p className='text-xs text-muted-foreground'>
        {info.isSuccess && `Server ${info.data.version}`}
        {info.isError && 'Server unreachable'}
      </p>
    </div>
  );
}
