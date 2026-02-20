/**
 * OverwriteDialog Component
 * Shows when a file transfer would overwrite an existing file
 * Supports: Overwrite, Skip, Rename, with "Apply to all" option
 */

import * as React from 'react';
import { useState, useEffect } from 'react';
import { AlertTriangle, File, Clock, HardDrive, ArrowRight, X, Check, SkipForward, Edit3 } from 'lucide-react';
import { useTranslation } from '../i18n';
import { formatBytes } from '../utils/formatters';

export type OverwriteAction = 'overwrite' | 'skip' | 'rename' | 'cancel';

export interface FileCompareInfo {
    name: string;
    size: number;
    modified?: Date | string | number;
    isRemote: boolean;
}

export interface OverwriteDialogProps {
    isOpen: boolean;
    source: FileCompareInfo;
    destination: FileCompareInfo;
    queueCount?: number; // Number of remaining files in queue
    onDecision: (action: OverwriteAction, applyToAll: boolean, newName?: string) => void;
    onCancel: () => void;
}

// Format date
const formatDate = (date: Date | string | number | undefined): string => {
    if (!date) return 'Unknown';
    const d = date instanceof Date ? date : new Date(date);
    if (isNaN(d.getTime())) return 'Unknown';
    return d.toLocaleString();
};

// Compare dates and return status
const getDateComparison = (source: Date | string | number | undefined, dest: Date | string | number | undefined): 'newer' | 'older' | 'same' | 'unknown' => {
    if (!source || !dest) return 'unknown';
    const srcDate = source instanceof Date ? source : new Date(source);
    const destDate = dest instanceof Date ? dest : new Date(dest);
    if (isNaN(srcDate.getTime()) || isNaN(destDate.getTime())) return 'unknown';

    const diff = srcDate.getTime() - destDate.getTime();
    if (Math.abs(diff) < 1000) return 'same'; // Within 1 second = same
    return diff > 0 ? 'newer' : 'older';
};

export const OverwriteDialog: React.FC<OverwriteDialogProps> = ({
    isOpen,
    source,
    destination,
    queueCount = 0,
    onDecision,
    onCancel,
}) => {
    const t = useTranslation();
    const [applyToAll, setApplyToAll] = useState(false);
    const [showRename, setShowRename] = useState(false);
    const [newFileName, setNewFileName] = useState('');

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    // Generate suggested new name
    const generateNewName = (name: string): string => {
        const dotIndex = name.lastIndexOf('.');
        if (dotIndex === -1) {
            return `${name}_copy`;
        }
        const baseName = name.substring(0, dotIndex);
        const ext = name.substring(dotIndex);
        return `${baseName}_copy${ext}`;
    };

    // Reset state when dialog opens
    React.useEffect(() => {
        if (isOpen && source) {
            setApplyToAll(false);
            setShowRename(false);
            setNewFileName(generateNewName(source.name));
        }
    }, [isOpen, source?.name]);

    // Early return if not open or missing data
    if (!isOpen || !source || !destination) return null;

    const dateComparison = getDateComparison(source.modified, destination.modified);
    const sizeDiff = source.size - destination.size;

    const handleOverwrite = () => onDecision('overwrite', applyToAll);
    const handleSkip = () => onDecision('skip', applyToAll);
    const handleRename = () => {
        if (newFileName && newFileName !== source.name) {
            onDecision('rename', applyToAll, newFileName);
        }
    };
    const handleCancel = () => {
        onDecision('cancel', false);
        onCancel();
    };

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center" role="dialog" aria-modal="true" aria-label="Overwrite Confirmation">
            {/* Backdrop */}
            <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" onClick={handleCancel} />

            {/* Dialog */}
            <div className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-md overflow-hidden">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <AlertTriangle size={18} className="text-amber-500" />
                        <span className="font-medium text-gray-900 dark:text-gray-100">
                            {t('overwrite.title') || 'File Already Exists'}
                        </span>
                        {queueCount > 0 && (
                            <span className="text-xs text-gray-500 ml-2">
                                +{queueCount} in queue
                            </span>
                        )}
                    </div>
                    <button
                        onClick={handleCancel}
                        className="p-1 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                    >
                        <X size={16} className="text-gray-400 hover:text-gray-200" />
                    </button>
                </div>

                {/* Content */}
                <div className="p-4 space-y-4">
                    {/* File name */}
                    <div className="text-center">
                        <div className="inline-flex items-center gap-2 px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg">
                            <File size={16} className="text-blue-500" />
                            <span className="font-medium text-gray-900 dark:text-white truncate max-w-[280px]">
                                {source.name}
                            </span>
                        </div>
                    </div>

                    {/* Comparison */}
                    <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-center">
                        {/* Source */}
                        <div className={`p-3 rounded-lg border ${source.isRemote ? 'border-green-300 dark:border-green-700 bg-green-50 dark:bg-green-900/30' : 'border-blue-300 dark:border-blue-700 bg-blue-50 dark:bg-blue-900/30'}`}>
                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">
                                {source.isRemote ? (t('overwrite.remote') || 'Remote') : (t('overwrite.local') || 'Local')}
                            </div>
                            <div className="space-y-1 text-sm">
                                <div className="flex items-center gap-1.5 text-gray-700 dark:text-gray-300">
                                    <HardDrive size={12} />
                                    <span>{formatBytes(source.size)}</span>
                                </div>
                                <div className="flex items-center gap-1.5 text-gray-600 dark:text-gray-400">
                                    <Clock size={12} />
                                    <span className="text-xs">{formatDate(source.modified)}</span>
                                </div>
                            </div>
                        </div>

                        {/* Arrow */}
                        <div className="flex flex-col items-center">
                            <ArrowRight size={20} className="text-gray-400" />
                        </div>

                        {/* Destination */}
                        <div className={`p-3 rounded-lg border ${!source.isRemote ? 'border-green-300 dark:border-green-700 bg-green-50 dark:bg-green-900/30' : 'border-blue-300 dark:border-blue-700 bg-blue-50 dark:bg-blue-900/30'}`}>
                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-2">
                                {destination.isRemote ? (t('overwrite.remote') || 'Remote') : (t('overwrite.local') || 'Local')}
                            </div>
                            <div className="space-y-1 text-sm">
                                <div className="flex items-center gap-1.5 text-gray-700 dark:text-gray-300">
                                    <HardDrive size={12} />
                                    <span>{formatBytes(destination.size)}</span>
                                </div>
                                <div className="flex items-center gap-1.5 text-gray-600 dark:text-gray-400">
                                    <Clock size={12} />
                                    <span className="text-xs">{formatDate(destination.modified)}</span>
                                </div>
                            </div>
                        </div>
                    </div>

                    {/* Date/Size comparison info */}
                    <div className="flex justify-center gap-4 text-xs">
                        {dateComparison !== 'unknown' && (
                            <span className={`px-2 py-1 rounded-full ${
                                dateComparison === 'newer' ? 'bg-green-100 dark:bg-green-900/50 text-green-700 dark:text-green-300' :
                                dateComparison === 'older' ? 'bg-red-100 dark:bg-red-900/50 text-red-700 dark:text-red-300' :
                                'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400'
                            }`}>
                                {dateComparison === 'newer' ? (t('overwrite.sourceNewer') || 'Source is newer') :
                                 dateComparison === 'older' ? (t('overwrite.sourceOlder') || 'Source is older') :
                                 (t('overwrite.sameDate') || 'Same date')}
                            </span>
                        )}
                        {sizeDiff !== 0 && (
                            <span className={`px-2 py-1 rounded-full ${
                                sizeDiff > 0 ? 'bg-blue-100 dark:bg-blue-900/50 text-blue-700 dark:text-blue-300' :
                                'bg-orange-100 dark:bg-orange-900/50 text-orange-700 dark:text-orange-300'
                            }`}>
                                {sizeDiff > 0 ? `+${formatBytes(sizeDiff)}` : `-${formatBytes(Math.abs(sizeDiff))}`}
                            </span>
                        )}
                    </div>

                    {/* Rename input */}
                    {showRename && (
                        <div className="space-y-2 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-lg border border-gray-200 dark:border-gray-600">
                            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300">
                                {t('overwrite.newName') || 'New file name:'}
                            </label>
                            <div className="flex gap-2">
                                <input
                                    type="text"
                                    value={newFileName}
                                    onChange={(e) => setNewFileName(e.target.value)}
                                    className="flex-1 px-3 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-800 focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                    autoFocus
                                />
                                <button
                                    onClick={handleRename}
                                    disabled={!newFileName || newFileName === source.name}
                                    className="px-4 py-2 bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded-lg text-sm font-medium transition-colors"
                                >
                                    {t('overwrite.rename') || 'Rename'}
                                </button>
                            </div>
                        </div>
                    )}

                    {/* Apply to all checkbox */}
                    {queueCount > 0 && (
                        <label className="flex items-center gap-2 p-2 bg-gray-50 dark:bg-gray-700/50 rounded-lg cursor-pointer hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors">
                            <input
                                type="checkbox"
                                checked={applyToAll}
                                onChange={(e) => setApplyToAll(e.target.checked)}
                                className="w-4 h-4 rounded border-gray-300 text-blue-500 focus:ring-blue-500"
                            />
                            <span className="text-sm text-gray-700 dark:text-gray-300">
                                {t('overwrite.applyToAll') || `Apply to all ${queueCount + 1} files`}
                            </span>
                        </label>
                    )}
                </div>

                {/* Actions */}
                <div className="px-4 pb-4 flex gap-2">
                    <button
                        onClick={handleOverwrite}
                        className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 bg-amber-500 hover:bg-amber-600 text-white rounded-lg font-medium transition-colors"
                    >
                        <Check size={16} />
                        {t('overwrite.overwrite') || 'Overwrite'}
                    </button>
                    <button
                        onClick={handleSkip}
                        className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200 rounded-lg font-medium transition-colors"
                    >
                        <SkipForward size={16} />
                        {t('overwrite.skip') || 'Skip'}
                    </button>
                    <button
                        onClick={() => setShowRename(!showRename)}
                        className={`flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg font-medium transition-colors ${
                            showRename
                                ? 'bg-blue-500 text-white'
                                : 'bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200'
                        }`}
                    >
                        <Edit3 size={16} />
                    </button>
                </div>
            </div>
        </div>
    );
};

export default OverwriteDialog;
