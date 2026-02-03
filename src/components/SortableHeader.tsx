/**
 * SortableHeader - Clickable table header for sorting file lists
 */

import React from 'react';
import { ArrowUp, ArrowDown } from 'lucide-react';

export type SortField = 'name' | 'size' | 'type' | 'modified';
export type SortOrder = 'asc' | 'desc';

interface SortableHeaderProps {
    label: string;
    field: SortField;
    currentField: SortField;
    order: SortOrder;
    onClick: (field: SortField) => void;
    className?: string;
}

export const SortableHeader: React.FC<SortableHeaderProps> = ({
    label,
    field,
    currentField,
    order,
    onClick,
    className = ''
}) => (
    <th
        className={`px-4 py-2 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-600 transition-colors select-none ${className}`}
        onClick={() => onClick(field)}
    >
        <div className="flex items-center gap-1">
            {label}
            {currentField === field && (
                order === 'asc'
                    ? <ArrowUp size={12} className="text-blue-500" />
                    : <ArrowDown size={12} className="text-blue-500" />
            )}
        </div>
    </th>
);

export default SortableHeader;
