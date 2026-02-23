
import React, { useState, useEffect, useCallback } from 'react';
import { X, Check, Shield } from 'lucide-react';
import { useTranslation } from '../i18n';

interface PermissionsDialogProps {
    isOpen: boolean;
    onClose: () => void;
    onSave: (mode: string) => void;
    fileName: string;
    currentPermissions?: string; // e.g. "drwxr-xr-x" or "755"
}

export const PermissionsDialog: React.FC<PermissionsDialogProps> = ({ isOpen, onClose, onSave, fileName, currentPermissions }) => {
    const t = useTranslation();
    const [octal, setOctal] = useState('755');
    const [flags, setFlags] = useState({
        owner: { read: true, write: true, execute: true },
        group: { read: true, write: false, execute: true },
        others: { read: true, write: false, execute: true }
    });

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    // Parse initial permissions
    useEffect(() => {
        if (isOpen && currentPermissions) {
            // If it looks like unix style "drwxr-xr-x"
            if (currentPermissions.length >= 10) {
                const p = currentPermissions.substring(1); // skip 'd' or '-'
                const newFlags = {
                    owner: {
                        read: p[0] === 'r',
                        write: p[1] === 'w',
                        execute: p[2] === 'x'
                    },
                    group: {
                        read: p[3] === 'r',
                        write: p[4] === 'w',
                        execute: p[5] === 'x'
                    },
                    others: {
                        read: p[6] === 'r',
                        write: p[7] === 'w',
                        execute: p[8] === 'x'
                    }
                };
                setFlags(newFlags);
                updateOctal(newFlags);
            }
            // If it looks like octal "644"
            else if (/^[0-7]{3}$/.test(currentPermissions)) {
                setOctal(currentPermissions);
                updateFlagsFromOctal(currentPermissions);
            }
        }
    }, [isOpen, currentPermissions]);

    const updateOctal = (currentFlags: typeof flags) => {
        const calc = (f: { read: boolean, write: boolean, execute: boolean }) =>
            (f.read ? 4 : 0) + (f.write ? 2 : 0) + (f.execute ? 1 : 0);

        const o = calc(currentFlags.owner);
        const g = calc(currentFlags.group);
        const ot = calc(currentFlags.others);
        setOctal(`${o}${g}${ot}`);
    };

    const updateFlagsFromOctal = (oct: string) => {
        if (!/^[0-7]{3}$/.test(oct)) return;

        const parseDigit = (d: string) => {
            const v = parseInt(d);
            return {
                read: (v & 4) !== 0,
                write: (v & 2) !== 0,
                execute: (v & 1) !== 0
            };
        };

        setFlags({
            owner: parseDigit(oct[0]),
            group: parseDigit(oct[1]),
            others: parseDigit(oct[2])
        });
    };

    const handleOctalChange = (e: React.ChangeEvent<HTMLInputElement>) => {
        const val = e.target.value;
        if (val.length <= 3) {
            setOctal(val);
            if (val.length === 3) updateFlagsFromOctal(val);
        }
    };

    const toggle = (section: 'owner' | 'group' | 'others', type: 'read' | 'write' | 'execute') => {
        const newFlags = { ...flags };
        newFlags[section][type] = !newFlags[section][type];
        setFlags(newFlags);
        updateOctal(newFlags);
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50 animate-fadeIn">
            <div className="bg-white dark:bg-gray-800 rounded-2xl p-6 shadow-2xl w-full max-w-md border border-gray-100 dark:border-gray-700 animate-scale-in">
                <div className="flex justify-between items-start mb-6">
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
                        <div className="flex justify-center"><input type="checkbox" checked={flags.owner.read} onChange={() => toggle('owner', 'read')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.owner.write} onChange={() => toggle('owner', 'write')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.owner.execute} onChange={() => toggle('owner', 'execute')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>

                        {/* Group */}
                        <div className="font-medium text-gray-700 dark:text-gray-300 flex items-center">{t('permissions.group')}</div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.group.read} onChange={() => toggle('group', 'read')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.group.write} onChange={() => toggle('group', 'write')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.group.execute} onChange={() => toggle('group', 'execute')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>

                        {/* Others */}
                        <div className="font-medium text-gray-700 dark:text-gray-300 flex items-center">{t('permissions.public')}</div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.others.read} onChange={() => toggle('others', 'read')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.others.write} onChange={() => toggle('others', 'write')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                        <div className="flex justify-center"><input type="checkbox" checked={flags.others.execute} onChange={() => toggle('others', 'execute')} className="w-5 h-5 rounded border-gray-300 text-blue-600 focus:ring-blue-500" /></div>
                    </div>

                    {/* Octal Input */}
                    <div className="bg-gray-50 dark:bg-gray-700/50 p-4 rounded-xl flex items-center justify-between">
                        <span className="text-sm font-medium text-gray-700 dark:text-gray-300">{t('permissions.octal')}</span>
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
                        <button onClick={onClose} className="flex-1 px-4 py-2.5 text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-xl font-medium transition-colors">
                            {t('common.cancel')}
                        </button>
                        <button onClick={() => onSave(octal)} className="flex-1 px-4 py-2.5 bg-blue-500 hover:bg-blue-600 text-white rounded-xl font-medium transition-colors flex items-center justify-center gap-2 shadow-lg shadow-blue-500/20">
                            <Check size={18} /> {t('permissions.apply')}
                        </button>
                    </div>
                </div>
            </div>
        </div>
    );
};
