import * as React from 'react';
import { useEffect } from 'react';
import { X, Keyboard } from 'lucide-react';
import { useTranslation } from '../i18n';

interface ShortcutsDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

export const ShortcutsDialog: React.FC<ShortcutsDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();

    const shortcuts = [
        {
            category: t('shortcuts.navigation'), items: [
                { keys: ['Enter'], action: t('shortcuts.openFolder') },
                { keys: ['Backspace'], action: t('shortcuts.goUp') },
                { keys: ['Tab'], action: t('shortcuts.switchPanel') },
                { keys: ['Ctrl', 'R'], action: t('shortcuts.refreshPanel') },
            ]
        },
        {
            category: t('shortcuts.fileOperations'), items: [
                { keys: ['Ctrl', 'U'], action: t('shortcuts.uploadSelected') },
                { keys: ['Ctrl', 'D'], action: t('shortcuts.downloadSelected') },
                { keys: ['Delete'], action: t('shortcuts.deleteSelected') },
                { keys: ['F2'], action: t('shortcuts.rename') },
                { keys: ['Ctrl', 'N'], action: t('shortcuts.newFolder') },
            ]
        },
        {
            category: t('shortcuts.selection'), items: [
                { keys: ['Ctrl', 'A'], action: t('browser.selectAll') },
                { keys: ['Ctrl', 'Click'], action: t('shortcuts.toggleSelection') },
                { keys: ['Shift', 'Click'], action: t('shortcuts.rangeSelection') },
            ]
        },
        {
            category: t('shortcuts.general'), items: [
                { keys: ['Ctrl', 'F'], action: t('shortcuts.focusSearch') },
                { keys: ['Ctrl', ','], action: t('shortcuts.openSettings') },
                { keys: ['F1'], action: t('shortcuts.showShortcuts') },
                { keys: ['Escape'], action: t('shortcuts.closeDialog') },
            ]
        },
    ];

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
            {/* Backdrop */}
            <div
                className="absolute inset-0 bg-black/50 backdrop-blur-sm"
                onClick={onClose}
            />

            {/* Dialog */}
            <div className="relative bg-white dark:bg-gray-800 rounded-2xl shadow-2xl w-full max-w-2xl max-h-[80vh] overflow-hidden animate-scale-in">
                {/* Header */}
                <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        <div className="p-2 bg-blue-100 dark:bg-blue-900/30 rounded-lg">
                            <Keyboard size={20} className="text-blue-500" />
                        </div>
                        <h2 className="text-lg font-semibold">{t('shortcuts.title')}</h2>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-2 text-gray-500 hover:text-gray-700 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={18} />
                    </button>
                </div>

                {/* Content */}
                <div className="p-4 overflow-y-auto max-h-[calc(80vh-80px)]">
                    <div className="grid md:grid-cols-2 gap-6">
                        {shortcuts.map(category => (
                            <div key={category.category}>
                                <h3 className="text-sm font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-3">
                                    {category.category}
                                </h3>
                                <div className="space-y-2">
                                    {category.items.map((shortcut, idx) => (
                                        <div
                                            key={idx}
                                            className="flex items-center justify-between py-1.5"
                                        >
                                            <span className="text-sm text-gray-700 dark:text-gray-300">
                                                {shortcut.action}
                                            </span>
                                            <div className="flex items-center gap-1">
                                                {shortcut.keys.map((key, kidx) => (
                                                    <React.Fragment key={kidx}>
                                                        <kbd className="px-2 py-1 text-xs font-medium bg-gray-100 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded shadow-sm">
                                                            {key}
                                                        </kbd>
                                                        {kidx < shortcut.keys.length - 1 && (
                                                            <span className="text-gray-400">+</span>
                                                        )}
                                                    </React.Fragment>
                                                ))}
                                            </div>
                                        </div>
                                    ))}
                                </div>
                            </div>
                        ))}
                    </div>
                </div>

                {/* Footer */}
                <div className="p-4 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
                    <p className="text-xs text-center text-gray-500">
                        {t('shortcuts.helpText')}
                    </p>
                </div>
            </div>
        </div>
    );
};

export default ShortcutsDialog;
