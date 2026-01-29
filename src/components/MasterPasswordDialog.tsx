/**
 * MasterPasswordDialog - Setup or unlock encrypted credential vault
 * Used on Linux without secret-service or when OS keyring is unavailable
 */

import React, { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Lock, Eye, EyeOff, AlertTriangle, Shield } from 'lucide-react';
import { useTranslation } from '../i18n';

interface MasterPasswordDialogProps {
    mode?: 'setup' | 'unlock';
    onComplete: () => void;
    onCancel: () => void;
}

export const MasterPasswordDialog: React.FC<MasterPasswordDialogProps> = ({
    mode = 'setup',
    onComplete,
    onCancel,
}) => {
    const t = useTranslation();
    const [password, setPassword] = useState('');
    const [confirm, setConfirm] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [error, setError] = useState('');
    const [loading, setLoading] = useState(false);

    const isSetup = mode === 'setup';
    const isValid = isSetup
        ? password.length >= 8 && password === confirm
        : password.length > 0;

    const handleSubmit = async () => {
        if (!isValid) return;
        setLoading(true);
        setError('');

        try {
            if (isSetup) {
                await invoke('setup_master_password', { password });
            } else {
                await invoke('unlock_vault', { password });
            }
            onComplete();
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
        } finally {
            setLoading(false);
        }
    };

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'Enter' && isValid) handleSubmit();
    };

    return (
        <div className="space-y-4">
            <div className="flex items-center gap-2 text-gray-700 dark:text-gray-200">
                <Lock size={18} className="text-blue-500" />
                <h3 className="font-medium">
                    {isSetup
                        ? (t('masterPassword.setupTitle') || 'Set Master Password')
                        : (t('masterPassword.unlockTitle') || 'Unlock Credential Vault')}
                </h3>
            </div>

            <p className="text-sm text-gray-500 dark:text-gray-400">
                {isSetup
                    ? (t('masterPassword.setupDesc') || 'Your OS keychain is not available. Set a master password to encrypt your credential vault locally.')
                    : (t('masterPassword.unlockDesc') || 'Enter your master password to unlock the credential vault.')}
            </p>

            {isSetup && (
                <div className="flex items-start gap-2 p-2.5 bg-amber-50 dark:bg-amber-900/20 rounded-lg border border-amber-200 dark:border-amber-700">
                    <AlertTriangle className="text-amber-500 flex-shrink-0 mt-0.5" size={16} />
                    <p className="text-xs text-amber-700 dark:text-amber-300">
                        {t('masterPassword.noRecovery') || 'There is no password recovery. If you forget this password, your stored credentials cannot be recovered.'}
                    </p>
                </div>
            )}

            <div className="space-y-3">
                <div className="relative">
                    <input
                        type={showPassword ? 'text' : 'password'}
                        value={password}
                        onChange={e => setPassword(e.target.value)}
                        onKeyDown={handleKeyDown}
                        placeholder={t('masterPassword.password') || 'Master password'}
                        className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-800 dark:text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                        autoFocus
                    />
                    <button
                        type="button"
                        onClick={() => setShowPassword(!showPassword)}
                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                    >
                        {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                    </button>
                </div>

                {isSetup && (
                    <input
                        type={showPassword ? 'text' : 'password'}
                        value={confirm}
                        onChange={e => setConfirm(e.target.value)}
                        onKeyDown={handleKeyDown}
                        placeholder={t('masterPassword.confirm') || 'Confirm password'}
                        className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-800 dark:text-gray-100 text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                    />
                )}

                {isSetup && password.length > 0 && password.length < 8 && (
                    <p className="text-xs text-red-500">{t('masterPassword.tooShort') || 'Minimum 8 characters'}</p>
                )}

                {isSetup && confirm.length > 0 && password !== confirm && (
                    <p className="text-xs text-red-500">{t('masterPassword.mismatch') || 'Passwords do not match'}</p>
                )}

                {error && (
                    <p className="text-xs text-red-500">{error}</p>
                )}
            </div>

            <div className="flex gap-3">
                <button
                    onClick={handleSubmit}
                    disabled={!isValid || loading}
                    className="flex-1 px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-400 text-white rounded-lg font-medium text-sm transition-colors flex items-center justify-center gap-2"
                >
                    <Shield size={14} />
                    {loading
                        ? (t('masterPassword.processing') || 'Processing...')
                        : isSetup
                            ? (t('masterPassword.create') || 'Create Vault')
                            : (t('masterPassword.unlock') || 'Unlock')}
                </button>
                <button
                    onClick={onCancel}
                    className="px-4 py-2 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200 rounded-lg font-medium text-sm transition-colors"
                >
                    {t('masterPassword.cancel') || 'Cancel'}
                </button>
            </div>
        </div>
    );
};

export default MasterPasswordDialog;
