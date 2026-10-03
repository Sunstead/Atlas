/**
 * The one way the app calls the server. Same origin, so the session cookie
 * rides along; types come from ts-rs (`src/generated/`, run
 * `cargo test -p atlas-common` to create them).
 */
import type { ApiError } from '@/generated/ApiError';

export class ApiRequestError extends Error {
  readonly status: number;
  /** The server's error body, or null when it sent something else (a proxy page). */
  readonly body: ApiError | null;

  constructor(status: number, body: ApiError | null) {
    super(body?.message ?? `Request failed (${status})`);
    this.status = status;
    this.body = body;
  }
}

function isApiError(v: unknown): v is ApiError {
  return typeof v === 'object' && v !== null && 'code' in v && 'message' in v;
}

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set('Accept', 'application/json');
  const method = (init.method ?? 'GET').toUpperCase();
  // The server refuses state-changing requests without this header, which a
  // cross-site form can't set.
  if (method !== 'GET' && method !== 'HEAD') headers.set('X-Atlas-Request', '1');

  const res = await fetch(path, { ...init, headers, credentials: 'same-origin' });
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {}

  if (!res.ok) throw new ApiRequestError(res.status, isApiError(data) ? data : null);
  return data as T;
}
