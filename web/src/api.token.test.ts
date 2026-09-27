// @vitest-environment happy-dom
// token 持久化（反馈轮 10）：存取/回填/失效清除。
import { afterEach, describe, expect, it } from 'vitest';
import { captureUrlToken, clearStoredToken, storeToken } from './api';

afterEach(() => {
  sessionStorage.clear();
  localStorage.clear();
  history.replaceState(null, '', location.pathname);
});

describe('token persistence', () => {
  it('storeToken writes both runtime and persistent layers', () => {
    storeToken('tok-1');
    expect(sessionStorage.getItem('hub_token')).toBe('tok-1');
    expect(localStorage.getItem('agent_hub_token')).toBe('tok-1');
  });

  it('captureUrlToken prefers the URL token and persists it', () => {
    localStorage.setItem('agent_hub_token', 'old');
    history.replaceState(null, '', '/?token=url-tok');
    expect(captureUrlToken()).toBe(true);
    expect(sessionStorage.getItem('hub_token')).toBe('url-tok');
    expect(localStorage.getItem('agent_hub_token')).toBe('url-tok');
  });

  it('captureUrlToken backfills the runtime layer from localStorage', () => {
    localStorage.setItem('agent_hub_token', 'saved-tok');
    expect(captureUrlToken()).toBe(true);
    expect(sessionStorage.getItem('hub_token')).toBe('saved-tok');
  });

  it('returns false when neither layer has a token', () => {
    expect(captureUrlToken()).toBe(false);
    expect(sessionStorage.getItem('hub_token')).toBeNull();
  });

  it('clearStoredToken wipes both layers (invalid-token path)', () => {
    storeToken('stale');
    clearStoredToken();
    expect(sessionStorage.getItem('hub_token')).toBeNull();
    expect(localStorage.getItem('agent_hub_token')).toBeNull();
  });
});
