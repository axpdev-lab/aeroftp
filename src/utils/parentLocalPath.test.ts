// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { isWindowsDriveRoot, parentLocalPath } from './parentLocalPath';

describe('isWindowsDriveRoot', () => {
  it('matches bare and slash-terminated drive roots', () => {
    expect(isWindowsDriveRoot('D:')).toBe(true);
    expect(isWindowsDriveRoot('D:/')).toBe(true);
    expect(isWindowsDriveRoot('D:\\')).toBe(true);
    expect(isWindowsDriveRoot('c:')).toBe(true);
    expect(isWindowsDriveRoot('C:/Users')).toBe(false);
    expect(isWindowsDriveRoot('/')).toBe(false);
    expect(isWindowsDriveRoot('/home')).toBe(false);
  });
});

describe('parentLocalPath', () => {
  it('returns absolute drive root from first-level folder (Windows bug fix)', () => {
    expect(parentLocalPath('D:/boot')).toBe('D:/');
    expect(parentLocalPath('D:\\boot')).toBe('D:/');
    expect(parentLocalPath('C:/Users')).toBe('C:/');
    expect(parentLocalPath('C:\\Windows')).toBe('C:/');
  });

  it('walks multi-segment paths', () => {
    expect(parentLocalPath('D:/boot/grub')).toBe('D:/boot');
    expect(parentLocalPath('C:/Users/axpne/Documents')).toBe('C:/Users/axpne');
  });

  it('stays at Windows drive root', () => {
    expect(parentLocalPath('D:')).toBe('D:/');
    expect(parentLocalPath('D:/')).toBe('D:/');
    expect(parentLocalPath('D:\\')).toBe('D:/');
  });

  it('handles Unix paths', () => {
    expect(parentLocalPath('/')).toBe('/');
    expect(parentLocalPath('/home')).toBe('/');
    expect(parentLocalPath('/home/user')).toBe('/home');
    expect(parentLocalPath('/home/user/docs')).toBe('/home/user');
  });

  it('handles empty', () => {
    expect(parentLocalPath('')).toBe('/');
  });
});
