/**
 * FileTagBadge — Finder-style colored dot badges next to filenames.
 *
 * Shows up to 3 color dots; overflow indicated with "+N".
 * React.memo for render performance in large file lists.
 */

import React from 'react';
import type { FileTag } from '../types/aerofile';

interface FileTagBadgeProps {
  tags: FileTag[];
  maxVisible?: number;
}

const FileTagBadgeInner: React.FC<FileTagBadgeProps> = ({ tags, maxVisible = 3 }) => {
  if (tags.length === 0) return null;

  const visible = tags.slice(0, maxVisible);
  const overflow = tags.length - maxVisible;

  return (
    <span className="inline-flex items-center gap-0.5 ml-1.5 shrink-0">
      {visible.map(tag => (
        <span
          key={tag.label_id}
          className="w-2 h-2 rounded-full shrink-0"
          style={{ backgroundColor: tag.label_color }}
          title={tag.label_name}
        />
      ))}
      {overflow > 0 && (
        <span className="text-[8px] text-gray-400 ml-0.5">+{overflow}</span>
      )}
    </span>
  );
};

export const FileTagBadge = React.memo(FileTagBadgeInner);

export default FileTagBadge;
