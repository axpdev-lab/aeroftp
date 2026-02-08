import React, { useState, useMemo, useCallback } from 'react';
import { Check, X, FileText, ChevronRight, ChevronDown } from 'lucide-react';
import { DiffPreview } from './DiffPreview';

export interface FileDiff {
    filePath: string;
    fileName: string;
    originalContent: string;
    modifiedContent: string;
    addedLines: number;
    removedLines: number;
}

interface MultiFileDiffPanelProps {
    diffs: FileDiff[];
    onApplySelected: (selectedPaths: string[]) => void;
    onCancel: () => void;
}

export const MultiFileDiffPanel: React.FC<MultiFileDiffPanelProps> = ({
    diffs,
    onApplySelected,
    onCancel,
}) => {
    const [selectedFiles, setSelectedFiles] = useState<Set<string>>(
        () => new Set(diffs.map(d => d.filePath))
    );
    const [expandedFile, setExpandedFile] = useState<string | null>(
        diffs.length > 0 ? diffs[0].filePath : null
    );

    const toggleFile = useCallback((path: string) => {
        setSelectedFiles(prev => {
            const next = new Set(prev);
            if (next.has(path)) next.delete(path);
            else next.add(path);
            return next;
        });
    }, []);

    const selectAll = useCallback(() => {
        setSelectedFiles(new Set(diffs.map(d => d.filePath)));
    }, [diffs]);

    const deselectAll = useCallback(() => {
        setSelectedFiles(new Set());
    }, []);

    const handleExpandToggle = useCallback((path: string) => {
        setExpandedFile(prev => prev === path ? null : path);
    }, []);

    const handleApply = useCallback(() => {
        onApplySelected(Array.from(selectedFiles));
    }, [onApplySelected, selectedFiles]);

    const totalAdded = useMemo(
        () => diffs.reduce((sum, d) => sum + d.addedLines, 0),
        [diffs]
    );

    const totalRemoved = useMemo(
        () => diffs.reduce((sum, d) => sum + d.removedLines, 0),
        [diffs]
    );

    const filesLabel = diffs.length !== 1 ? 'files' : 'file';
    const applyLabel = selectedFiles.size !== 1
        ? `Apply ${selectedFiles.size} files`
        : `Apply ${selectedFiles.size} file`;

    return (
        <div className="border border-gray-700/50 rounded-lg overflow-hidden bg-gray-900/80 my-3">
            {/* Header */}
            <div className="flex items-center justify-between px-3 py-2 bg-gray-800/60 border-b border-gray-700/50">
                <div className="flex items-center gap-2 text-xs">
                    <FileText size={13} className="text-purple-400" />
                    <span className="font-medium text-gray-200">
                        {diffs.length} {filesLabel} changed
                    </span>
                    <span className="text-green-400">+{totalAdded}</span>
                    <span className="text-red-400">-{totalRemoved}</span>
                </div>
                <div className="flex items-center gap-1.5">
                    <button
                        onClick={selectAll}
                        className="text-[10px] text-gray-500 hover:text-gray-300"
                    >
                        Select all
                    </button>
                    <span className="text-gray-700">|</span>
                    <button
                        onClick={deselectAll}
                        className="text-[10px] text-gray-500 hover:text-gray-300"
                    >
                        Deselect all
                    </button>
                </div>
            </div>

            {/* File list */}
            <div className="max-h-[400px] overflow-y-auto">
                {diffs.map(diff => (
                    <div key={diff.filePath}>
                        {/* File row */}
                        <div
                            className="flex items-center gap-2 px-3 py-1.5 hover:bg-gray-800/40 cursor-pointer border-b border-gray-700/30"
                            onClick={() => handleExpandToggle(diff.filePath)}
                        >
                            {/* Checkbox */}
                            <input
                                type="checkbox"
                                checked={selectedFiles.has(diff.filePath)}
                                onChange={(e) => {
                                    e.stopPropagation();
                                    toggleFile(diff.filePath);
                                }}
                                className="rounded border-gray-600 bg-gray-700 text-purple-500 focus:ring-purple-500 focus:ring-offset-0 h-3 w-3"
                            />
                            {/* Expand arrow */}
                            {expandedFile === diff.filePath
                                ? <ChevronDown size={12} className="text-gray-500" />
                                : <ChevronRight size={12} className="text-gray-500" />
                            }
                            {/* File name */}
                            <span className="text-xs font-mono text-gray-300 flex-1 truncate">
                                {diff.fileName}
                            </span>
                            {/* Stats */}
                            <span className="text-[10px] text-green-400">+{diff.addedLines}</span>
                            <span className="text-[10px] text-red-400">-{diff.removedLines}</span>
                        </div>
                        {/* Expanded diff */}
                        {expandedFile === diff.filePath && (
                            <div className="px-2 py-1 bg-gray-900/40">
                                <DiffPreview
                                    originalContent={diff.originalContent}
                                    modifiedContent={diff.modifiedContent}
                                    fileName={diff.fileName}
                                    showActions={false}
                                />
                            </div>
                        )}
                    </div>
                ))}
            </div>

            {/* Footer actions */}
            <div className="flex items-center justify-between px-3 py-2 border-t border-gray-700/50 bg-gray-800/40">
                <span className="text-[10px] text-gray-500">
                    {selectedFiles.size} of {diffs.length} selected
                </span>
                <div className="flex items-center gap-2">
                    <button
                        onClick={onCancel}
                        className="flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium bg-gray-700/80 hover:bg-gray-600 text-gray-300 transition-colors"
                    >
                        <X size={10} />
                        Cancel
                    </button>
                    <button
                        onClick={handleApply}
                        disabled={selectedFiles.size === 0}
                        className="flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium bg-green-600/80 hover:bg-green-500 text-white transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                        <Check size={10} />
                        {applyLabel}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default MultiFileDiffPanel;
