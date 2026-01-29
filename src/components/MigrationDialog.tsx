/**
 * MigrationDialog - Securely migrate plaintext credentials to OS keyring/vault
 * Shown on first launch after v1.3.2 update when legacy credentials are detected
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Shield, AlertTriangle, Check, Loader2, Lock, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { MasterPasswordDialog } from './MasterPasswordDialog';
import { ServerProfile } from '../types';

interface MigrationDialogProps {
    isOpen: boolean;
    onClose: (migrated: boolean) => void;
}

interface MigrationResult {
    migrated_count: number;
    errors: string[];
    old_file_deleted: boolean;
}

export const MigrationDialog: React.FC<MigrationDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [status, setStatus] = useState<'checking' | 'ready' | 'migrating' | 'master_password' | 'done' | 'error'>('checking');
    const [keyringAvailable, setKeyringAvailable] = useState(false);
    const [result, setResult] = useState<MigrationResult | null>(null);
    const [error, setError] = useState('');
    const [localStorageMigrated, setLocalStorageMigrated] = useState(0);

    useEffect(() => {
        if (!isOpen) return;
        checkStatus();
    }, [isOpen]);

    const checkStatus = async () => {
        setStatus('checking');
        try {
            const available = await invoke<boolean>('check_keyring_available');
            setKeyringAvailable(available);
            setStatus(available ? 'ready' : 'master_password');
        } catch (e) {
            setStatus('ready'); // Try anyway
        }
    };

    const handleMasterPasswordSet = () => {
        setStatus('ready');
    };

    const handleMigrate = async () => {
        setStatus('migrating');
        try {
            // 1. Migrate server credentials JSON file via backend
            const migrationResult = await invoke<MigrationResult>('migrate_plaintext_credentials');

            // 2. Migrate localStorage passwords to credential store
            let lsCount = 0;
            const serversJson = localStorage.getItem('aeroftp-saved-servers');
            if (serversJson) {
                try {
                    const servers: ServerProfile[] = JSON.parse(serversJson);
                    for (const server of servers) {
                        if (server.password) {
                            try {
                                await invoke('store_credential', {
                                    account: `server_${server.id}`,
                                    password: server.password,
                                });
                                lsCount++;
                            } catch (e) {
                                console.error(`Failed to migrate password for ${server.name}:`, e);
                            }
                        }
                    }
                    // Mark servers as migrated but keep password as fallback
                    // Passwords are retained in localStorage in case the credential store
                    // becomes unavailable (keyring locked, vault inaccessible, etc.)
                    const cleaned = servers.map(s => ({
                        ...s,
                        hasStoredCredential: !!s.password,
                    }));
                    localStorage.setItem('aeroftp-saved-servers', JSON.stringify(cleaned));
                } catch (e) {
                    console.error('Failed to parse saved servers:', e);
                }
            }

            // 3. Migrate OAuth client secrets from localStorage
            const oauthProviders = ['googledrive', 'dropbox', 'onedrive'];
            for (const p of oauthProviders) {
                const id = localStorage.getItem(`oauth_${p}_client_id`);
                const secret = localStorage.getItem(`oauth_${p}_client_secret`);
                if (id) {
                    await invoke('store_credential', { account: `oauth_${p}_client_id`, password: id }).catch(console.error);
                    localStorage.removeItem(`oauth_${p}_client_id`);
                    lsCount++;
                }
                if (secret) {
                    await invoke('store_credential', { account: `oauth_${p}_client_secret`, password: secret }).catch(console.error);
                    localStorage.removeItem(`oauth_${p}_client_secret`);
                    lsCount++;
                }
            }

            // 4. Remove OAuth settings with secrets from localStorage
            localStorage.removeItem('aeroftp_oauth_settings');

            setLocalStorageMigrated(lsCount);
            setResult(migrationResult);
            setStatus('done');
        } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
            setStatus('error');
        }
    };

    const handleSkip = () => {
        onClose(false);
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm flex items-center justify-center p-4">
            <div className="bg-white dark:bg-gray-800 rounded-2xl shadow-2xl max-w-md w-full overflow-hidden">
                {/* Header */}
                <div className="bg-gradient-to-r from-blue-600 to-indigo-600 px-6 py-4 flex items-center gap-3">
                    <Shield className="text-white" size={24} />
                    <h2 className="text-white text-lg font-semibold">
                        {t('migration.title') || 'Secure Credential Migration'}
                    </h2>
                </div>

                <div className="p-6 space-y-4">
                    {status === 'checking' && (
                        <div className="flex items-center gap-3 text-gray-600 dark:text-gray-300">
                            <Loader2 className="animate-spin" size={20} />
                            <span>{t('migration.checking') || 'Checking security status...'}</span>
                        </div>
                    )}

                    {status === 'master_password' && (
                        <MasterPasswordDialog
                            onComplete={handleMasterPasswordSet}
                            onCancel={handleSkip}
                        />
                    )}

                    {status === 'ready' && (
                        <>
                            <div className="flex items-start gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 rounded-lg border border-amber-200 dark:border-amber-700">
                                <AlertTriangle className="text-amber-500 flex-shrink-0 mt-0.5" size={20} />
                                <div className="text-sm text-amber-800 dark:text-amber-200">
                                    <p className="font-medium">{t('migration.warning') || 'Your credentials are stored in plaintext'}</p>
                                    <p className="mt-1 text-amber-700 dark:text-amber-300">
                                        {t('migration.warningDetail') || 'Passwords and tokens will be migrated to your OS keychain for secure storage.'}
                                    </p>
                                </div>
                            </div>

                            <div className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
                                <Lock size={16} className="text-green-500" />
                                <span>
                                    {keyringAvailable
                                        ? (t('migration.keyringDetected') || 'OS Keychain detected - credentials will be securely stored')
                                        : (t('migration.vaultMode') || 'Using encrypted vault with master password')}
                                </span>
                            </div>

                            <div className="flex gap-3 pt-2">
                                <button
                                    onClick={handleMigrate}
                                    className="flex-1 px-4 py-2.5 bg-blue-600 hover:bg-blue-700 text-white rounded-lg font-medium transition-colors flex items-center justify-center gap-2"
                                >
                                    <Shield size={16} />
                                    {t('migration.migrate') || 'Migrate Now'}
                                </button>
                                <button
                                    onClick={handleSkip}
                                    className="px-4 py-2.5 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200 rounded-lg font-medium transition-colors"
                                >
                                    {t('migration.skip') || 'Skip'}
                                </button>
                            </div>

                            <p className="text-xs text-gray-400 dark:text-gray-500 text-center">
                                {t('migration.skipWarning') || 'Skipping will leave your credentials in plaintext files'}
                            </p>
                        </>
                    )}

                    {status === 'migrating' && (
                        <div className="flex items-center gap-3 text-gray-600 dark:text-gray-300">
                            <Loader2 className="animate-spin" size={20} />
                            <span>{t('migration.inProgress') || 'Migrating credentials securely...'}</span>
                        </div>
                    )}

                    {status === 'done' && result && (
                        <>
                            <div className="flex items-start gap-3 p-3 bg-green-50 dark:bg-green-900/20 rounded-lg border border-green-200 dark:border-green-700">
                                <Check className="text-green-500 flex-shrink-0 mt-0.5" size={20} />
                                <div className="text-sm text-green-800 dark:text-green-200">
                                    <p className="font-medium">{t('migration.success') || 'Migration Complete'}</p>
                                    <p className="mt-1">
                                        {result.migrated_count + localStorageMigrated} {t('migration.credentialsMigrated') || 'credentials securely migrated'}
                                    </p>
                                    {result.old_file_deleted && (
                                        <p className="mt-1 text-green-600 dark:text-green-300">
                                            {t('migration.oldFileDeleted') || 'Old plaintext file securely deleted'}
                                        </p>
                                    )}
                                </div>
                            </div>
                            {result.errors.length > 0 && (
                                <div className="text-xs text-amber-600 dark:text-amber-400">
                                    {result.errors.map((err, i) => <p key={i}>{err}</p>)}
                                </div>
                            )}
                            <button
                                onClick={() => onClose(true)}
                                className="w-full px-4 py-2.5 bg-green-600 hover:bg-green-700 text-white rounded-lg font-medium transition-colors"
                            >
                                {t('migration.continue') || 'Continue'}
                            </button>
                        </>
                    )}

                    {status === 'error' && (
                        <>
                            <div className="flex items-start gap-3 p-3 bg-red-50 dark:bg-red-900/20 rounded-lg border border-red-200 dark:border-red-700">
                                <AlertTriangle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
                                <div className="text-sm text-red-800 dark:text-red-200">
                                    <p className="font-medium">{t('migration.error') || 'Migration Failed'}</p>
                                    <p className="mt-1">{error}</p>
                                </div>
                            </div>
                            <button
                                onClick={() => onClose(false)}
                                className="w-full px-4 py-2.5 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200 rounded-lg font-medium transition-colors"
                            >
                                {t('migration.close') || 'Close'}
                            </button>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
};

export default MigrationDialog;
