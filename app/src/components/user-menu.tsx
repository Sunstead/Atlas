import { Link } from '@tanstack/react-router';
import { LogOut, Settings } from 'lucide-react';
import { Button, buttonVariants } from '@sunstead/ui/components/button';
import { displayName, signOut, useMe } from '@/lib/auth';

/** Who's signed in, settings, and sign out. */
export function UserMenu() {
  const me = useMe();
  if (!me.data) return null;

  return (
    <div className='flex items-center gap-1'>
      <span className='hidden max-w-40 truncate px-1 text-sm text-muted-foreground sm:inline' title={me.data.username}>
        {displayName(me.data)}
      </span>
      <Link
        to='/settings/connections'
        aria-label='Settings'
        title='Settings'
        className={buttonVariants({ variant: 'ghost', size: 'icon' })}
      >
        <Settings />
      </Link>
      <Button variant='ghost' size='icon' aria-label='Sign out' title='Sign out' onClick={() => void signOut()}>
        <LogOut />
      </Button>
    </div>
  );
}
