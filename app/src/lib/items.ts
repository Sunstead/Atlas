import { useQuery } from '@tanstack/react-query';
import { api } from './api';
import type { ItemInfo } from '@/generated/ItemInfo';
import type { ItemRef } from '@/generated/ItemRef';
import type { PreviewInfo } from '@/generated/PreviewInfo';
import type { SearchResponse } from '@/generated/SearchResponse';
import type { SearchParams } from './search-params';

/** Search as the URL describes it. Nothing runs without a query. */
export function useSearchResults(params: SearchParams) {
  const q = params.q?.trim() ?? '';
  return useQuery({
    queryKey: ['search', q, params.type ?? null, params.source ?? null],
    queryFn: () => {
      const p = new URLSearchParams({ q });
      if (params.type) p.set('type', params.type);
      if (params.source) p.set('source', params.source);
      return api<SearchResponse>(`/v1/search?${p}`);
    },
    enabled: q.length > 0,
    // Keep the old results on screen while the next query loads.
    placeholderData: (previous) => previous,
  });
}

const itemPath = (ref: Pick<ItemRef, 'connection' | 'id'>) => `/v1/items/${ref.connection}/${ref.id}`;

export function useItem(ref: Pick<ItemRef, 'connection' | 'id'> | null) {
  return useQuery({
    queryKey: ['item', ref?.connection, ref?.id],
    queryFn: () => api<ItemInfo>(itemPath(ref!)),
    enabled: ref !== null,
  });
}

export function usePreview(ref: Pick<ItemRef, 'connection' | 'id'> | null) {
  return useQuery({
    queryKey: ['preview', ref?.connection, ref?.id],
    queryFn: () => api<PreviewInfo>(`${itemPath(ref!)}/preview`),
    enabled: ref !== null,
  });
}

/** The item's bytes, for `<img>`, `<video>` and downloads. The session cookie authenticates it. */
export function blobUrl(ref: Pick<ItemRef, 'connection' | 'id'>, opts: { download?: boolean; thumbnail?: boolean } = {}) {
  const p = new URLSearchParams();
  if (opts.download) p.set('download', '1');
  if (opts.thumbnail) p.set('variant', 'thumbnail');
  const qs = p.toString();
  return `${itemPath(ref)}/blob${qs ? `?${qs}` : ''}`;
}

/** `?preview=` holds `<connection>:<id>`; the id is base64url, so it has no colon. */
export function previewParam(ref: Pick<ItemRef, 'connection' | 'id'>): string {
  return `${ref.connection}:${ref.id}`;
}

export function parsePreviewParam(value: string | undefined): Pick<ItemRef, 'connection' | 'id'> | null {
  const m = value?.match(/^(\d+):([A-Za-z0-9_-]+)$/);
  return m ? { connection: Number(m[1]), id: m[2] } : null;
}
