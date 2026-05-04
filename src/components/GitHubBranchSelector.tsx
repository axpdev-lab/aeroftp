// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * GitHubBranchSelector Component
 * Compact toolbar dropdown for viewing and switching GitHub branches.
 * Shows current branch, protected status, and write mode context.
 */

import React, { useState, useRef, useEffect, useCallback } from 'react';
import { GitBranch, ChevronDown, Lock, Check, RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';

interface GitHubBranchSelectorProps {
  currentBranch: string;
  branches: Array<{ name: string; protected: boolean }>;
  writeMode: 'direct' | 'branch' | 'readonly';
  workingBranch?: string;
  onBranchChange: (branch: string) => void;
  onRefresh: () => void;
}

export const GitHubBranchSelector: React.FC<GitHubBranchSelectorProps> = ({
  currentBranch,
  branches,
  writeMode,
  workingBranch,
  onBranchChange,
  onRefresh,
}) => {
  const t = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(-1);
  const dropdownRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Click outside to close
  useEffect(() => {
    if (!isOpen) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setIsOpen(false);
        setFocusedIndex(-1);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [isOpen]);

  // Keyboard navigation
  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (!isOpen) {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'ArrowDown') {
        e.preventDefault();
        setIsOpen(true);
        setFocusedIndex(0);
      }
      return;
    }

    switch (e.key) {
      case 'Escape':
        e.preventDefault();
        setIsOpen(false);
        setFocusedIndex(-1);
        break;
      case 'ArrowDown':
        e.preventDefault();
        setFocusedIndex(prev => Math.min(prev + 1, branches.length - 1));
        break;
      case 'ArrowUp':
        e.preventDefault();
        setFocusedIndex(prev => Math.max(prev - 1, 0));
        break;
      case 'Enter':
        e.preventDefault();
        if (focusedIndex >= 0 && focusedIndex < branches.length) {
          const selected = branches[focusedIndex];
          if (selected.name !== currentBranch) {
            onBranchChange(selected.name);
          }
          setIsOpen(false);
          setFocusedIndex(-1);
        }
        break;
    }
  }, [isOpen, focusedIndex, branches, currentBranch, onBranchChange]);

  // Scroll focused item into view
  useEffect(() => {
    if (focusedIndex >= 0 && listRef.current) {
      const items = listRef.current.querySelectorAll('[data-branch-item]');
      items[focusedIndex]?.scrollIntoView({ block: 'nearest' });
    }
  }, [focusedIndex]);

  const handleToggle = () => {
    setIsOpen(prev => !prev);
    if (!isOpen) {
      setFocusedIndex(-1);
    }
  };

  const handleSelect = (branchName: string) => {
    if (branchName !== currentBranch) {
      onBranchChange(branchName);
    }
    setIsOpen(false);
    setFocusedIndex(-1);
  };

  const handleRefresh = (e: React.MouseEvent) => {
    e.stopPropagation();
    onRefresh();
  };

  return (
    <div ref={dropdownRef} className="relative inline-flex" onKeyDown={handleKeyDown}>
      {/* Trigger button */}
      <button
        onClick={handleToggle}
        className="flex items-center gap-1.5 px-2 py-1 text-xs rounded-lg border transition-colors hover:opacity-80"
        style={{
          borderColor: 'var(--color-border)',
          color: 'var(--color-text-primary)',
          backgroundColor: 'var(--color-bg-secondary)',
        }}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        title={t('github.switchBranch') || 'Switch branch'}
      >
        <GitBranch size={12} style={{ color: 'var(--color-accent)' }} />
        <span className="max-w-[120px] truncate font-medium">{currentBranch}</span>
        <ChevronDown
          size={12}
          className={`transition-transform ${isOpen ? 'rotate-180' : ''}`}
          style={{ color: 'var(--color-text-secondary)' }}
        />
      </button>

      {/* Dropdown */}
      {isOpen && (
        <div
          className="absolute top-full left-0 mt-1 w-56 rounded-lg border shadow-xl overflow-hidden z-50 animate-scale-in"
          style={{
            backgroundColor: 'var(--color-bg-secondary)',
            borderColor: 'var(--color-border)',
          }}
          role="listbox"
          aria-label={t('github.branchList') || 'Branch list'}
        >
          {/* Header */}
          <div
            className="flex items-center justify-between px-3 py-2 border-b"
            style={{ borderColor: 'var(--color-border)' }}
          >
            <span
              className="text-xs font-medium"
              style={{ color: 'var(--color-text-secondary)' }}
            >
              Branches
              <span className="ml-1 opacity-60">({branches.length})</span>
            </span>
            <button
              onClick={handleRefresh}
              className="p-1 rounded transition-colors hover:opacity-80"
              style={{ color: 'var(--color-text-secondary)' }}
              title="Refresh branches"
            >
              <RefreshCw size={12} />
            </button>
          </div>

          {/* Branch list */}
          <div
            ref={listRef}
            className="max-h-60 overflow-y-auto py-1"
          >
            {branches.length === 0 ? (
              <div
                className="px-3 py-4 text-xs text-center"
                style={{ color: 'var(--color-text-secondary)' }}
              >
                {t('github.noBranches') || 'No branches found'}
              </div>
            ) : (
              branches.map((b, index) => {
                const isCurrent = b.name === currentBranch;
                const isWorking = writeMode === 'branch' && b.name === workingBranch;
                const isFocused = index === focusedIndex;

                return (
                  <button
                    key={b.name}
                    data-branch-item
                    onClick={() => handleSelect(b.name)}
                    className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-left transition-colors"
                    style={{
                      color: 'var(--color-text-primary)',
                      backgroundColor: isFocused ? 'var(--color-bg-primary)' : undefined,
                    }}
                    onMouseEnter={() => setFocusedIndex(index)}
                    role="option"
                    aria-selected={isCurrent}
                  >
                    {/* Check mark for current branch */}
                    <span className="w-3 flex-shrink-0">
                      {isCurrent && <Check size={12} style={{ color: 'var(--color-accent)' }} />}
                    </span>

                    {/* Branch name */}
                    <span
                      className={`flex-1 truncate ${isCurrent ? 'font-semibold' : ''}`}
                      style={isCurrent ? { color: 'var(--color-accent)' } : undefined}
                    >
                      {b.name}
                    </span>

                    {/* Working branch indicator */}
                    {isWorking && !isCurrent && (
                      <span className="px-1 py-0.5 rounded text-[10px] font-medium text-yellow-500 bg-yellow-500/10">
                        {t('github.working') || 'working'}
                      </span>
                    )}

                    {/* Protected badge */}
                    {b.protected && (
                      <Lock
                        size={10}
                        className="flex-shrink-0 text-amber-500"
                        aria-label={t('github.protectedBranch') || 'Protected branch'}
                      />
                    )}
                  </button>
                );
              })
            )}
          </div>
        </div>
      )}
    </div>
  );
};
