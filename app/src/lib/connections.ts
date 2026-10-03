import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api } from './api';
import type { ConnectionInfo } from '@/generated/ConnectionInfo';
import type { CreateConnection } from '@/generated/CreateConnection';
import type { SourceKindInfo } from '@/generated/SourceKindInfo';
import type { UpdateConnection } from '@/generated/UpdateConnection';

export function useSourceKinds() {
  return useQuery({ queryKey: ['source-kinds'], queryFn: () => api<SourceKindInfo[]>('/v1/source-kinds') });
}

export function useConnections() {
  return useQuery({ queryKey: ['connections'], queryFn: () => api<ConnectionInfo[]>('/v1/connections') });
}

const json = (body: unknown): RequestInit => ({
  body: JSON.stringify(body),
  headers: { 'Content-Type': 'application/json' },
});

/** Create, update and remove, each refreshing the list when it lands. */
export function useConnectionMutations() {
  const qc = useQueryClient();
  const onSuccess = () => qc.invalidateQueries({ queryKey: ['connections'] });

  return {
    create: useMutation({
      mutationFn: (req: CreateConnection) =>
        api<ConnectionInfo>('/v1/connections', { method: 'POST', ...json(req) }),
      onSuccess,
    }),
    update: useMutation({
      mutationFn: ({ id, ...req }: UpdateConnection & { id: number }) =>
        api<ConnectionInfo>(`/v1/connections/${id}`, { method: 'PATCH', ...json(req) }),
      onSuccess,
    }),
    remove: useMutation({
      mutationFn: (id: number) => api<null>(`/v1/connections/${id}`, { method: 'DELETE' }),
      onSuccess,
    }),
  };
}
