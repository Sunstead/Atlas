import { useQuery } from '@tanstack/react-query';
import {
  Activity,
  BookUser,
  CalendarDays,
  FolderOpen,
  GitBranch,
  Globe,
  Images,
  KeyRound,
  NotebookPen,
  type LucideIcon,
} from 'lucide-react';
import { api } from '@/lib/api';
import type { AppLink } from '@/generated/AppLink';

const ICONS: Record<string, LucideIcon> = {
  photos: Images,
  files: FolderOpen,
  notes: NotebookPen,
  git: GitBranch,
  monitor: Activity,
  contacts: BookUser,
  calendar: CalendarDays,
  auth: KeyRound,
};

/** The apps on Jupiter, one tap away. */
export function Launcher() {
  const apps = useQuery({ queryKey: ['apps'], queryFn: () => api<AppLink[]>('/v1/apps'), staleTime: 10 * 60_000 });
  if (!apps.data?.length) return null;

  return (
    <nav aria-label='Apps' className='grid w-full max-w-2xl grid-cols-3 gap-2 sm:grid-cols-4'>
      {apps.data.map((app) => {
        const Icon = ICONS[app.icon ?? ''] ?? Globe;
        return (
          <a
            key={app.url}
            href={app.url}
            target='_blank'
            rel='noreferrer'
            className='flex flex-col items-center gap-1.5 rounded-xl px-2 py-3 text-center transition-colors hover:bg-muted'
          >
            <span className='flex size-10 items-center justify-center rounded-xl bg-muted'>
              <Icon className='size-5' />
            </span>
            <span className='w-full truncate text-sm font-medium'>{app.name}</span>
            {app.description && <span className='-mt-1 w-full truncate text-xs text-muted-foreground'>{app.description}</span>}
          </a>
        );
      })}
    </nav>
  );
}
