/**
 * useFileTags — hook for managing file tags via the Rust file_tags backend.
 *
 * Provides label management, batch tag queries, and tag operations.
 * Uses a Map cache for quick lookups by file path.
 */

import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { TagLabel, FileTag, LabelCount } from '../types/aerofile';

interface UseFileTagsReturn {
  // Labels
  labels: TagLabel[];
  labelCounts: LabelCount[];
  loadLabels: () => Promise<void>;
  createLabel: (name: string, color: string) => Promise<TagLabel | null>;
  updateLabel: (id: number, name: string, color: string) => Promise<void>;
  deleteLabel: (id: number) => Promise<void>;

  // Tags
  tagsCache: Map<string, FileTag[]>;
  loadTagsForFiles: (paths: string[]) => Promise<void>;
  setTags: (filePaths: string[], labelIds: number[]) => Promise<void>;
  removeTag: (filePath: string, labelId: number) => Promise<void>;
  getTagsForFile: (path: string) => FileTag[];

  // Filter
  activeTagFilter: number | null;
  setActiveTagFilter: (labelId: number | null) => void;
}

export function useFileTags(): UseFileTagsReturn {
  const [labels, setLabels] = useState<TagLabel[]>([]);
  const [labelCounts, setLabelCounts] = useState<LabelCount[]>([]);
  const [tagsCache, setTagsCache] = useState<Map<string, FileTag[]>>(new Map());
  const [activeTagFilter, setActiveTagFilter] = useState<number | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clear debounce timer on unmount to prevent state updates on unmounted component (M29)
  useEffect(() => {
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
        debounceRef.current = null;
      }
    };
  }, []);

  // Load all labels on mount
  const loadLabels = useCallback(async () => {
    try {
      const [labelsResult, countsResult] = await Promise.all([
        invoke<TagLabel[]>('file_tags_list_labels'),
        invoke<LabelCount[]>('file_tags_get_label_counts'),
      ]);
      setLabels(labelsResult);
      setLabelCounts(countsResult);
    } catch (e) {
      console.warn('Failed to load file tag labels:', e);
    }
  }, []);

  useEffect(() => {
    loadLabels();
  }, [loadLabels]);

  // Batch-load tags for visible files (debounced)
  const loadTagsForFiles = useCallback(async (paths: string[]) => {
    if (paths.length === 0) return;

    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(async () => {
      try {
        const result = await invoke<FileTag[]>('file_tags_get_tags_for_files', { filePaths: paths });
        const newCache = new Map<string, FileTag[]>();
        // Initialize all requested paths
        for (const p of paths) newCache.set(p, []);
        // Fill in results
        for (const tag of result) {
          const existing = newCache.get(tag.file_path) || [];
          existing.push(tag);
          newCache.set(tag.file_path, existing);
        }
        setTagsCache(prev => {
          const merged = new Map(prev);
          for (const [k, v] of newCache) merged.set(k, v);
          return merged;
        });
      } catch (e) {
        console.warn('Failed to load file tags:', e);
      }
    }, 150);
  }, []);

  // Get tags for a single file from cache
  const getTagsForFile = useCallback((path: string): FileTag[] => {
    return tagsCache.get(path) || [];
  }, [tagsCache]);

  // Set tags (batch) — assigns label_ids to all file_paths
  const setTags = useCallback(async (filePaths: string[], labelIds: number[]) => {
    try {
      await invoke('file_tags_set_tags', { filePaths, labelIds });
      // Refresh cache for affected files
      await loadTagsForFiles(filePaths);
      // Refresh counts
      const counts = await invoke<LabelCount[]>('file_tags_get_label_counts');
      setLabelCounts(counts);
    } catch (e) {
      console.warn('Failed to set file tags:', e);
    }
  }, [loadTagsForFiles]);

  // Remove a single tag
  const removeTag = useCallback(async (filePath: string, labelId: number) => {
    try {
      await invoke('file_tags_remove_tag', { filePath, labelId });
      // Refresh cache
      await loadTagsForFiles([filePath]);
      // Refresh counts
      const counts = await invoke<LabelCount[]>('file_tags_get_label_counts');
      setLabelCounts(counts);
    } catch (e) {
      console.warn('Failed to remove file tag:', e);
    }
  }, [loadTagsForFiles]);

  // Create a new custom label
  const createLabel = useCallback(async (name: string, color: string): Promise<TagLabel | null> => {
    try {
      const label = await invoke<TagLabel>('file_tags_create_label', { name, color });
      await loadLabels();
      return label;
    } catch (e) {
      console.warn('Failed to create label:', e);
      return null;
    }
  }, [loadLabels]);

  // Update label name/color
  const updateLabel = useCallback(async (id: number, name: string, color: string) => {
    try {
      await invoke('file_tags_update_label', { id, name, color });
      await loadLabels();
    } catch (e) {
      console.warn('Failed to update label:', e);
    }
  }, [loadLabels]);

  // Delete label (cascades to file_tags)
  const deleteLabel = useCallback(async (id: number) => {
    try {
      await invoke('file_tags_delete_label', { id });
      if (activeTagFilter === id) setActiveTagFilter(null);
      await loadLabels();
      // Clear cache entries that had this label
      setTagsCache(prev => {
        const newCache = new Map<string, FileTag[]>();
        for (const [path, tags] of prev) {
          newCache.set(path, tags.filter(t => t.label_id !== id));
        }
        return newCache;
      });
    } catch (e) {
      console.warn('Failed to delete label:', e);
    }
  }, [activeTagFilter, loadLabels]);

  return {
    labels,
    labelCounts,
    loadLabels,
    createLabel,
    updateLabel,
    deleteLabel,
    tagsCache,
    loadTagsForFiles,
    setTags,
    removeTag,
    getTagsForFile,
    activeTagFilter,
    setActiveTagFilter,
  };
}
