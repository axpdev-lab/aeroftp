// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)


import React, { useState, useEffect } from 'react';
import { X, Check, Shield } from 'lucide-react';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { flagsFromOctal, isCompleteOctal, octalFromFlags, parsePermissions, toggleFlag, type PermissionFlags, type PermissionTriple } from './permissionsMode';

interface PermissionsDialogProps {
    isOpen: boolean;
    onClose: () => void;
    onSave: (mode: string) => void;
    fileName: string;
    currentPermissions?: string; // e.g. "drwxr-xr-x" or "755"
}

/**
 * Mount one instance per opening (the caller keys it): the starting mode is
 * read from `currentPermissions` once, so a value edited for one file can
 * never carry over to the next one.
 */
export const PermissionsDialog: React.FC<PermissionsDialogProps> = ({ isOpen, onClose, onSave, fileName, currentPermissions }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [initial] = useState(() => parsePermissions(currentPermissions));
    const [octal, setOctal] = useState(initial.octal);
    const [flags, setFlags] = useState<PermissionFlags>(initial.flags);

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    const handleOctalChange = (e: React.ChangeEvent<HTMLInputElement>) => {
        const val = e.target.value;
        if (val.length <= 3 && /^[0-7]*$/.test(val)) {
            setOctal(val);
            if (isCompleteOctal(val)) setFlags(flagsFromOctal(val));
        }
    };

    const toggle = (section: keyof PermissionFlags, kind: keyof PermissionTriple) => {
        const next = toggleFlag(flags, section, kind);
        setFlags(next);
        setOctal(octalFromFlags(next));
    };

    const complete = isCompleteOctal(octal);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50 animate-fadeIn">
            <div {...modalDrag.panelProps} className="bg-white dark:bg-gray-800 rounded-lg p-6 shadow-2xl w-full max-w-md border border-gray-100 dark:border-gray-700 animate-scale-in">
                <div {...modalDrag.dragHandleProps} className="flex justify-between items-start mb-6 cursor-grab active:cursor-grabbing">
                    <div>
                        <h3 className="text-xl font-bold text-gray-900 dark:text-white flex items-center gap-2">
                            <Shield className="text-blue-500" size={24} />
                            {t('permissions.title')}
                        </h3>
                        <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
                            {fileName}
                        </p>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors" title={t('common.close')}>
                        <X size={20} className="text-gray-500" />
                    </button>
                </div>

                <div className="space-y-6">
                    {/* Grid */}
                    <div className="grid grid-cols-4 gap-4 text-sm">
                        <div className="font-medium text-gray-500"></div>
                        <div className="font-medium text-gray-900 dark:text-gray-100 text-center">{t('permissions.read')}</div>
                        <div className="font-medium text-gray-900 dark:text-gray-100 text-center">{t('permissions.write')}</div>
                        <div className="font-medium text-gray-900 dark:text-gray-100 text-center">{t('permissions.execute')}</div>

                        {/* Owner */}
                        <div className="font-medium text-gray-700 dark:text-gray-300 flex items-center">{t('permissions.owner')}</div>
                        <div className="flex justify-center"><Checkbox checked={flags.owner.read} onChange={() => toggle('owner', 'read')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.owner.write} onChange={() => toggle('owner', 'write')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.owner.execute} onChange={() => toggle('owner', 'execute')} /></div>

                        {/* Group */}
                        <div className="font-medium text-gray-700 dark:text-gray-300 flex items-center">{t('permissions.group')}</div>
                        <div className="flex justify-center"><Checkbox checked={flags.group.read} onChange={() => toggle('group', 'read')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.group.write} onChange={() => toggle('group', 'write')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.group.execute} onChange={() => toggle('group', 'execute')} /></div>

                        {/* Others */}
                        <div className="font-medium text-gray-700 dark:text-gray-300 flex items-center">{t('permissions.public')}</div>
                        <div className="flex justify-center"><Checkbox checked={flags.others.read} onChange={() => toggle('others', 'read')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.others.write} onChange={() => toggle('others', 'write')} /></div>
                        <div className="flex justify-center"><Checkbox checked={flags.others.execute} onChange={() => toggle('others', 'execute')} /></div>
                    </div>

                    {/* Octal Input */}
                    <div className="bg-gray-50 dark:bg-gray-700/50 p-4 rounded-lg flex items-center justify-between">
                        <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                            {t('permissions.octal')}
                            {!initial.known && <span className="block text-xs font-normal text-amber-600 dark:text-amber-400 mt-1">{t('permissions.currentUnknown')}</span>}
                        </span>
                        <input
                            type="text"
                            value={octal}
                            onChange={handleOctalChange}
                            className="w-24 px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-center font-mono font-medium focus:ring-2 focus:ring-blue-500 outline-none"
                            maxLength={3}
                        />
                    </div>

                    {/* Actions */}
                    <div className="flex gap-3 pt-2">
                        <button onClick={onClose} className="flex-1 px-4 py-2.5 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg font-medium transition-colors">
                            {t('common.cancel')}
                        </button>
                        <button onClick={() => { if (complete) onSave(octal); }} disabled={!complete} className="flex-1 px-4 py-2.5 bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded-lg font-medium transition-colors flex items-center justify-center gap-2 shadow-lg shadow-blue-500/20">
                            <Check size={18} /> {t('permissions.apply')}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};
