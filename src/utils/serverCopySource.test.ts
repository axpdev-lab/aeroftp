import { describe, expect, it } from 'vitest';
import { serverCopySource } from './serverCopySource';

describe('serverCopySource', () => {
  it('sends an S3 folder with its trailing slash', () => {
    expect(serverCopySource('/docs', true, 's3', false)).toBe('/docs/');
    expect(serverCopySource('/docs/', true, 's3', false)).toBe('/docs/');
  });
  it('leaves files, other protocols, the crypt overlay and the root alone', () => {
    expect(serverCopySource('/docs', false, 's3', false)).toBe('/docs');
    expect(serverCopySource('/docs', true, 'webdav', false)).toBe('/docs');
    expect(serverCopySource('/docs', true, 's3', true)).toBe('/docs');
    expect(serverCopySource('/', true, 's3', false)).toBe('/');
  });
});
