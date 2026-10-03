import { Link, useRouterState } from '@tanstack/react-router';
import { Compass } from 'lucide-react';
import { SearchBox } from '@/components/search-box';
import { ThemeSelect } from '@/components/theme-select';
import { UserMenu } from '@/components/user-menu';

/** The shell: a header with the search field everywhere but home, then the page. */
export function AppLayout({ children }: { children: React.ReactNode }) {
  const location = useRouterState({ select: (s) => s.location });
  const onHome = location.pathname === '/';
  const q = location.pathname === '/search' ? (location.search as { q?: string }).q ?? '' : '';

  return (
    <div className='flex min-h-dvh flex-col'>
      <header className='flex h-14 shrink-0 items-center gap-3 border-b px-4'>
        <Link to='/' className='flex items-center gap-2 font-heading font-semibold'>
          <Compass className='size-5' />
          <span>Atlas</span>
        </Link>
        <div className='mx-auto w-full max-w-2xl'>{!onHome && <SearchBox key={q} initial={q} />}</div>
        <ThemeSelect />
        <UserMenu />
      </header>
      <main className='flex flex-1 flex-col'>{children}</main>
    </div>
  );
}
