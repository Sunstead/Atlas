/**
 * `/search` params. Every view is reachable by URL, so a search can be
 * bookmarked, shared or started from the browser's address bar
 * (`/search?q=%s`).
 */
export interface SearchParams {
  q?: string;
  /** Restrict to one kind of item, e.g. `file` or `photo`. */
  type?: string;
  /** Restrict to one connection. */
  source?: string;
  /** The result open in the preview pane, as `<connection>:<id>`. */
  preview?: string;
}

const str = (v: unknown) => (typeof v === 'string' && v !== '' ? v : undefined);

export function parseSearchParams(s: Record<string, unknown>): SearchParams {
  // Numbers too: the router parses `?q=42` into 42.
  const q = typeof s.q === 'number' ? String(s.q) : str(s.q);
  return { q, type: str(s.type), source: str(s.source), preview: str(s.preview) };
}
