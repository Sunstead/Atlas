import { useQuery } from '@tanstack/react-query';
import { api } from './api';
import type { Me } from '@/generated/Me';
import type { LogoutResponse } from '@/generated/LogoutResponse';

/** The signed-in user. A 401 sends the browser to sign in (see `api`). */
export function useMe() {
  return useQuery({ queryKey: ['me'], queryFn: () => api<Me>('/v1/me'), staleTime: 5 * 60_000 });
}

export function displayName(me: Me): string {
  return me.display_name || me.username;
}

/** Ends the session here, then at the identity provider if it has a page for that. */
export async function signOut(): Promise<void> {
  const { redirect } = await api<LogoutResponse>('/auth/logout', { method: 'POST' });
  window.location.assign(redirect);
}
