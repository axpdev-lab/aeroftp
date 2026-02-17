/**
 * FolderOverwriteDialog Component
 * Shown when transferring a folder that already exists at the destination.
 * Offers merge strategies instead of simple overwrite/skip.
 */

import * as React from 'react';
import { Folder, AlertTriangle, X, Layers, Clock, FileCheck, SkipForward } from 'lucide-react';
import { useTranslation } from '../i18n';

export type FolderMergeAction = 'merge_overwrite' | 'merge_skip_identical' | 'merge_overwrite_newer' | 'skip' | 'cancel';

export interface FolderOverwriteDialogProps {
    isOpen: boolean;
    folderName: string;
    direction: 'upload' | 'download';
    queueCount?: number;
    onDecision: (action: FolderMergeAction, applyToAll: boolean) => void;
    onCancel: () => void;
}

export const FolderOverwriteDialog: React.FC<FolderOverwriteDialogProps> = ({
    isOpen,
    folderName,
    direction,
    queueCount = 0,
    onDecision,
    onCancel,
}) => {
    const t = useTranslation();
    const [applyToAll, setApplyToAll] = React.useState(false);

    React.useEffect(() => {
        if (isOpen) setApplyToAll(false);
    }, [isOpen]);

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    React.useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    if (!isOpen) return null;

    const handleCancel = () => {
        onDecision('cancel', false);
        onCancel();
    };

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
            <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" onClick={handleCancel} />

            <div className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-md overflow-hidden">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <AlertTriangle size={18} className="text-amber-500" />
                        <span className="font-medium text-gray-900 dark:text-gray-100">
                            {t('folderOverwrite.title') || 'Folder Already Exists'}
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
                    {/* Folder name */}
                    <div className="text-center">
                        <div className="inline-flex items-center gap-2 px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg">
                            <Folder size={16} className="text-amber-500" />
                            <span className="font-medium text-gray-900 dark:text-white truncate max-w-[280px]">
                                {folderName}
                            </span>
                        </div>
                    </div>

                    <p className="text-sm text-gray-600 dark:text-gray-400 text-center">
                        {t('folderOverwrite.description') || 'The destination folder already exists. Choose how to handle files inside:'}
                    </p>

                    {/* Action buttons */}
                    <div className="space-y-2">
                        <button
                            onClick={() => onDecision('merge_overwrite', applyToAll)}
                            className="w-full flex items-center gap-3 px-4 py-3 bg-amber-50 dark:bg-amber-900/20 hover:bg-amber-100 dark:hover:bg-amber-900/40 border border-amber-200 dark:border-amber-800 rounded-lg transition-colors text-left"
                        >
                            <Layers size={18} className="text-amber-600 dark:text-amber-400 shrink-0" />
                            <div>
                                <div className="text-sm font-medium text-gray-900 dark:text-white">
                                    {t('folderOverwrite.mergeOverwrite') || 'Merge & Overwrite all files'}
                                </div>
                            </div>
                        </button>

                        <button
                            onClick={() => onDecision('merge_skip_identical', applyToAll)}
                            className="w-full flex items-center gap-3 px-4 py-3 bg-green-50 dark:bg-green-900/20 hover:bg-green-100 dark:hover:bg-green-900/40 border border-green-200 dark:border-green-800 rounded-lg transition-colors text-left"
                        >
                            <FileCheck size={18} className="text-green-600 dark:text-green-400 shrink-0" />
                            <div>
                                <div className="text-sm font-medium text-gray-900 dark:text-white">
                                    {t('folderOverwrite.mergeSkipIdentical') || 'Merge & Skip identical files'}
                                </div>
                            </div>
                        </button>

                        <button
                            onClick={() => onDecision('merge_overwrite_newer', applyToAll)}
                            className="w-full flex items-center gap-3 px-4 py-3 bg-blue-50 dark:bg-blue-900/20 hover:bg-blue-100 dark:hover:bg-blue-900/40 border border-blue-200 dark:border-blue-800 rounded-lg transition-colors text-left"
                        >
                            <Clock size={18} className="text-blue-600 dark:text-blue-400 shrink-0" />
                            <div>
                                <div className="text-sm font-medium text-gray-900 dark:text-white">
                                    {t('folderOverwrite.mergeOverwriteNewer') || 'Merge & Overwrite if newer'}
                                </div>
                            </div>
                        </button>

                        <button
                            onClick={() => onDecision('skip', applyToAll)}
                            className="w-full flex items-center gap-3 px-4 py-3 bg-gray-50 dark:bg-gray-700/50 hover:bg-gray-100 dark:hover:bg-gray-700 border border-gray-200 dark:border-gray-600 rounded-lg transition-colors text-left"
                        >
                            <SkipForward size={18} className="text-gray-500 dark:text-gray-400 shrink-0" />
                            <div>
                                <div className="text-sm font-medium text-gray-900 dark:text-white">
                                    {t('folderOverwrite.skipFolder') || 'Skip folder'}
                                </div>
                            </div>
                        </button>
                    </div>

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
                                {t('overwrite.applyToAll') || `Apply to all ${queueCount + 1} items`}
                            </span>
                        </label>
                    )}
                </div>
            </div>
        </div>
    );
};

export default FolderOverwriteDialog;
