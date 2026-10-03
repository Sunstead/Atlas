import { describe, expect, it, vi } from 'vitest';
import { api, ApiRequestError, shouldRetry, signInUrl } from './api';

function respond(status: number, body: string) {
  // A fresh Response per call: a body can only be read once.
  return vi.spyOn(globalThis, 'fetch').mockImplementation(async () => new Response(body, { status }));
}

/** The error a failing call throws. */
async function failure(path: string): Promise<ApiRequestError> {
  try {
    await api(path);
  } catch (e) {
    if (e instanceof ApiRequestError) return e;
    throw e;
  }
  throw new Error('expected the call to fail');
}

describe('api', () => {
  it('parses JSON on success', async () => {
    respond(200, '{"version":"0.1.0","api_version":1}');
    await expect(api('/v1/info')).resolves.toEqual({ version: '0.1.0', api_version: 1 });
  });

  it('throws the server error body', async () => {
    respond(404, '{"code":"not_found","message":"Not found","detail":null}');
    const err = await failure('/v1/nope');
    expect(err.status).toBe(404);
    expect(err.body?.code).toBe('not_found');
  });

  it('survives a non-JSON error page', async () => {
    respond(502, '<html>Bad gateway</html>');
    const err = await failure('/v1/info');
    expect(err.status).toBe(502);
    expect(err.body).toBeNull();
  });

  it('marks state-changing requests', async () => {
    const fetch = respond(200, '{}');
    await api('/v1/x', { method: 'POST' });
    await api('/v1/x');
    const headers = (i: number) => new Headers(fetch.mock.calls[i][1]!.headers);
    expect(headers(0).get('X-Atlas-Request')).toBe('1');
    expect(headers(1).get('X-Atlas-Request')).toBeNull();
  });

  it('sends a 401 to sign-in, coming back to this page', async () => {
    const assign = vi.fn();
    vi.stubGlobal('location', { pathname: '/search', search: '?q=tax 2025', assign });
    respond(401, '{"code":"unauthorized","message":"Sign in to continue","detail":null}');
    const err = await failure('/v1/me');
    expect(err.status).toBe(401);
    expect(assign).toHaveBeenCalledWith('/auth/login?return_to=%2Fsearch%3Fq%3Dtax%202025');
    vi.unstubAllGlobals();
  });
});

describe('signInUrl', () => {
  it('encodes the whole path and query', () => {
    expect(signInUrl({ pathname: '/search', search: '?q=a&type=file' })).toBe(
      '/auth/login?return_to=%2Fsearch%3Fq%3Da%26type%3Dfile',
    );
  });
});

describe('shouldRetry', () => {
  it('retries server trouble once, never a 4xx', () => {
    expect(shouldRetry(0, new ApiRequestError(503, null))).toBe(true);
    expect(shouldRetry(1, new ApiRequestError(503, null))).toBe(false);
    expect(shouldRetry(0, new TypeError('network'))).toBe(true);
    expect(shouldRetry(0, new ApiRequestError(401, null))).toBe(false);
    expect(shouldRetry(0, new ApiRequestError(404, null))).toBe(false);
  });
});
