import React, { useMemo } from 'react';
import { Check, X, FileText } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface DiffPreviewProps {
    originalContent: string;
    modifiedContent: string;
    fileName: string;
    onApply?: () => void;
    onCancel?: () => void;
    showActions?: boolean;
}

/** Compute a simple unified diff between two texts */
function computeUnifiedDiff(original: string, modified: string): DiffLine[] {
    const origLines = original.split('\n');
    const modLines = modified.split('\n');
    const diff: DiffLine[] = [];

    // Simple LCS-based diff
    const n = origLines.length;
    const m = modLines.length;

    // For performance with large files, use a simple approach
    // Walk through lines, matching where possible
    let i = 0;
    let j = 0;

    while (i < n || j < m) {
        if (i < n && j < m && origLines[i] === modLines[j]) {
            diff.push({ type: 'context', content: origLines[i], lineOrig: i + 1, lineMod: j + 1 });
            i++;
            j++;
        } else {
            // Look ahead for matching lines
            let foundOrig = -1;
            let foundMod = -1;
            const lookAhead = Math.min(10, Math.max(n - i, m - j));

            for (let k = 1; k <= lookAhead; k++) {
                if (j + k < m && i < n && origLines[i] === modLines[j + k]) {
                    foundMod = j + k;
                    break;
                }
                if (i + k < n && j < m && origLines[i + k] === modLines[j]) {
                    foundOrig = i + k;
                    break;
                }
            }

            if (foundMod >= 0) {
                // Lines added in modified
                while (j < foundMod) {
                    diff.push({ type: 'add', content: modLines[j], lineMod: j + 1 });
                    j++;
                }
            } else if (foundOrig >= 0) {
                // Lines removed from original
                while (i < foundOrig) {
                    diff.push({ type: 'remove', content: origLines[i], lineOrig: i + 1 });
                    i++;
                }
            } else {
                // Both different — show as remove + add
                if (i < n) {
                    diff.push({ type: 'remove', content: origLines[i], lineOrig: i + 1 });
                    i++;
                }
                if (j < m) {
                    diff.push({ type: 'add', content: modLines[j], lineMod: j + 1 });
                    j++;
                }
            }
        }
    }

    return diff;
}

interface DiffLine {
    type: 'context' | 'add' | 'remove';
    content: string;
    lineOrig?: number;
    lineMod?: number;
}

export const DiffPreview: React.FC<DiffPreviewProps> = ({
    originalContent,
    modifiedContent,
    fileName,
    onApply,
    onCancel,
    showActions,
}) => {
    const t = useTranslation();

    const diffLines = useMemo(
        () => computeUnifiedDiff(originalContent, modifiedContent),
        [originalContent, modifiedContent]
    );

    const addCount = diffLines.filter(l => l.type === 'add').length;
    const removeCount = diffLines.filter(l => l.type === 'remove').length;

    return (
        <div className="border border-gray-700/50 rounded-lg overflow-hidden bg-gray-900/60 my-2">
            {/* Header */}
            <div className="flex items-center gap-2 px-3 py-1.5 bg-gray-800/60 border-b border-gray-700/50 text-xs">
                <FileText size={12} className="text-purple-400" />
                <span className="font-mono text-gray-300">{fileName}</span>
                <span className="ml-auto flex items-center gap-2">
                    {addCount > 0 && <span className="text-green-400">+{addCount}</span>}
                    {removeCount > 0 && <span className="text-red-400">-{removeCount}</span>}
                </span>
            </div>

            {/* Diff content */}
            <div className="max-h-60 overflow-auto text-[11px] font-mono leading-relaxed">
                {diffLines.map((line, idx) => {
                    const bg = line.type === 'add' ? 'bg-green-900/20' :
                               line.type === 'remove' ? 'bg-red-900/20' : '';
                    const textColor = line.type === 'add' ? 'text-green-300' :
                                      line.type === 'remove' ? 'text-red-300' : 'text-gray-500';
                    const prefix = line.type === 'add' ? '+' : line.type === 'remove' ? '-' : ' ';
                    const lineNum = line.type === 'remove' ? line.lineOrig : line.lineMod;

                    return (
                        <div key={idx} className={`flex ${bg} hover:bg-gray-800/40`}>
                            <span className="w-8 text-right pr-2 text-gray-600 select-none shrink-0 border-r border-gray-700/30">
                                {lineNum || ''}
                            </span>
                            <span className={`px-1 ${line.type === 'add' ? 'text-green-400' : line.type === 'remove' ? 'text-red-400' : 'text-gray-600'} select-none`}>
                                {prefix}
                            </span>
                            <span className={`flex-1 ${textColor} whitespace-pre`}>
                                {line.content}
                            </span>
                        </div>
                    );
                })}
            </div>

            {/* Footer actions */}
            {(showActions ?? true) && onApply && onCancel && (
                <div className="flex items-center gap-2 px-3 py-2 border-t border-gray-700/50 bg-gray-800/40">
                    <button
                        onClick={onApply}
                        className="flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium bg-green-600/80 hover:bg-green-500 text-white transition-colors"
                    >
                        <Check size={10} />
                        {t('ai.diff.apply') || 'Apply'}
                    </button>
                    <button
                        onClick={onCancel}
                        className="flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium bg-gray-700/80 hover:bg-gray-600 text-gray-300 transition-colors"
                    >
                        <X size={10} />
                        {t('ai.diff.cancel') || 'Cancel'}
                    </button>
                </div>
            )}
        </div>
    );
};

export default DiffPreview;
