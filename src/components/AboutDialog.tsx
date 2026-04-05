// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useMemo } from 'react';
import { X, Mail, Copy, Check, ExternalLink, Heart } from 'lucide-react';

const Github = ({ size = 24 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor">
    <path fillRule="evenodd" clipRule="evenodd" d="M12.026 2c-5.509 0-9.974 4.465-9.974 9.974 0 4.406 2.857 8.145 6.821 9.465.499.09.679-.217.679-.481 0-.237-.008-.865-.011-1.696-2.775.602-3.361-1.338-3.361-1.338-.452-1.152-1.107-1.459-1.107-1.459-.905-.619.069-.605.069-.605 1.002.07 1.527 1.028 1.527 1.028.89 1.524 2.336 1.084 2.902.829.091-.645.351-1.085.635-1.334-2.214-.251-4.542-1.107-4.542-4.93 0-1.087.389-1.979 1.024-2.675-.101-.253-.446-1.268.099-2.64 0 0 .837-.269 2.742 1.021a9.582 9.582 0 0 1 2.496-.336 9.554 9.554 0 0 1 2.496.336c1.906-1.291 2.742-1.021 2.742-1.021.545 1.372.203 2.387.099 2.64.64.696 1.024 1.587 1.024 2.675 0 3.833-2.33 4.675-4.552 4.922.355.308.675.916.675 1.846 0 1.334-.012 2.41-.012 2.737 0 .267.178.577.687.479C19.146 20.115 22 16.379 22 11.974 22 6.465 17.535 2 12.026 2z"/>
  </svg>
);

const SourceForge = ({ size = 24 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 117 103" fill="currentColor">
    <path d="M46.2 94.8c-.4 0-.9-.2-1.2-.5L.5 49.8c-.6-.6-.6-1.7 0-2.4l47-47C47.8.2 48.2 0 48.6 0h13.5c.8 0 1.3.5 1.5 1s.2 1.2-.4 1.8L19.1 47c-.9.9-.9 2.3 0 3.2l34.9 35c.6.6.6 1.7 0 2.4l-6.7 6.8c-.3.2-.7.4-1.1.4z" />
    <path d="M55.1 102.6c-.8 0-1.3-.5-1.5-1s-.2-1.2.4-1.8L98.2 55.6c.4-.4.7-1 .7-1.6s-.2-1.2-.7-1.6l-35-35c-.6-.6-.6-1.7 0-2.4L70 8.2c.3-.3.7-.5 1.2-.5s.8.3 1.1.6l44.4 44.5c.3.3.5.7.5 1.2s-.2.9-.5 1.2l-47 47c-.3.3-.7.5-1.2.5H55.1z" />
    <path d="M67 54.2c0-5-1.8-7.4-2.8-8.2-.2-.2-.5 0-.4.2.2 2.9-3.4 3.6-3.4 8v.1c0 2.7 2 4.9 4.6 4.9s4.6-2.2 4.6-4.9v-.1c0-1.3-.5-2.5-1-3.4-.1-.2-.4-.1-.3.1.8 3.8-1.3 6.2-1.3 3.3z" />
  </svg>
);
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { openUrl } from '../utils/openUrl';

interface AboutDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

interface SystemInfo {
    app_version: string;
    os: string;
    os_version: string;
    arch: string;
    tauri_version: string;
    rust_version: string;
    keyring_backend: string;
    config_dir: string;
    vault_exists: boolean;
    known_hosts_exists: boolean;
    dep_versions: Record<string, string>;
}

type TabId = 'info' | 'technical' | 'support';

// Key dependencies to display in technical tab (versions come from backend)
const KEY_DEPENDENCY_LABELS: { name: string; description: string }[] = [
    { name: 'russh', description: 'SSH/SFTP' },
    { name: 'russh-sftp', description: 'SFTP ops' },
    { name: 'suppaftp', description: 'FTP/FTPS' },
    { name: 'reqwest', description: 'HTTP' },
    { name: 'keyring', description: 'OS Keyring' },
    { name: 'aes-gcm', description: 'AES-256-GCM' },
    { name: 'argon2', description: 'KDF' },
    { name: 'zip', description: 'ZIP archives' },
    { name: 'sevenz-rust', description: '7z AES-256' },
    { name: 'quick-xml', description: 'WebDAV' },
    { name: 'oauth2', description: 'OAuth2' },
];

// Injected at build time by Vite (see vite.config.ts define)
declare const __FRONTEND_VERSIONS__: { react: string; typescript: string; tailwindcss: string; monaco: string; vite: string };
const _fv = typeof __FRONTEND_VERSIONS__ !== 'undefined' ? __FRONTEND_VERSIONS__ : { react: '?', typescript: '?', tailwindcss: '?', monaco: '?', vite: '?' };
const FRONTEND_DEPS = [
    { name: 'React', version: _fv.react },
    { name: 'TypeScript', version: _fv.typescript },
    { name: 'Tailwind CSS', version: _fv.tailwindcss },
    { name: 'Monaco Editor', version: _fv.monaco },
    { name: 'Vite', version: _fv.vite },
];


// Info row helper
const InfoRow: React.FC<{ label: string; value: string | React.ReactNode; mono?: boolean }> = ({ label, value, mono = true }) => (
    <div className="flex justify-between items-start py-1.5 border-b border-gray-200/50 dark:border-gray-800/50 last:border-0">
        <span className="text-xs text-gray-500 shrink-0">{label}</span>
        <span className={`text-xs text-gray-700 dark:text-gray-300 text-right ${mono ? 'font-mono' : ''}`}>{value}</span>
    </div>
);

export const AboutDialog: React.FC<AboutDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [activeTab, setActiveTab] = useState<TabId>('info');
    const [appVersion, setAppVersion] = useState('0.0.0');
    const [systemInfo, setSystemInfo] = useState<SystemInfo | null>(null);
    const [copied, setCopied] = useState(false);

    // Hide scrollbars when dialog is open (WebKitGTK fix)
    useEffect(() => {
        if (isOpen) {
            document.documentElement.classList.add('modal-open');
            return () => { document.documentElement.classList.remove('modal-open'); };
        }
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        getVersion().then(setAppVersion).catch(() => setAppVersion('1.3.4'));
        invoke<SystemInfo>('get_system_info').then(setSystemInfo).catch(() => {});
        // Reset state on open
        setActiveTab('info');
        setCopied(false);
    }, [isOpen]);

    const tabs: { id: TabId; label: string }[] = [
        { id: 'info', label: t('about.tabs.info') },
        { id: 'technical', label: t('about.tabs.technical') },
        { id: 'support', label: t('about.tabs.support') },
    ];

    const technicalText = useMemo(() => {
        if (!systemInfo) return '';
        const lines = [
            `AeroFTP ${appVersion}`,
            '',
            `--- ${t('about.buildInfo')} ---`,
            `Tauri: ${systemInfo.tauri_version}`,
            `Rust: ${systemInfo.rust_version}`,
            ...FRONTEND_DEPS.map(d => `${d.name}: ${d.version}`),
            '',
            `--- ${t('about.systemDetails')} ---`,
            `${t('about.operatingSystem')}: ${systemInfo.os}`,
            `${t('about.architecture')}: ${systemInfo.arch}`,
            `${t('about.keyringBackend')}: ${systemInfo.keyring_backend}`,
            `${t('about.configDir')}: ${systemInfo.config_dir}`,
            `${t('about.vaultStatus')}: ${systemInfo.vault_exists ? t('about.active') : t('about.inactive')}`,
            `${t('about.knownHosts')}: ${systemInfo.known_hosts_exists ? t('about.found') : t('about.notFound')}`,
            '',
            `--- ${t('about.linkedLibraries')} ---`,
            ...KEY_DEPENDENCY_LABELS.map(d => `${d.name}: ${systemInfo.dep_versions?.[d.name] ?? '?'} (${d.description})`),
        ];
        return lines.join('\n');
    }, [systemInfo, appVersion, t]);

    const copyTechnicalInfo = () => {
        navigator.clipboard.writeText(technicalText);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
            <div className="absolute inset-0 bg-black/50" onClick={onClose} />

            <div className="relative bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-2xl w-full max-w-lg overflow-hidden flex flex-col animate-scale-in" style={{ maxHeight: '85vh' }}>
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 shrink-0">
                    <div className="flex items-center gap-2.5">
                        <img
                            src="/icons/AeroFTP_simbol_color_512x512.png"
                            alt="AeroFTP"
                            className="w-6 h-6 object-contain"
                        />
                        <h2 className="text-base font-semibold font-mono">AeroFTP</h2>
                    </div>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" title={t('common.close')}>
                        <X size={16} />
                    </button>
                </div>

                {/* Tab bar */}
                <div className="flex border-b border-gray-200 dark:border-gray-700 shrink-0">
                    {tabs.map(tab => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id)}
                            className={`flex-1 px-4 py-2.5 text-sm font-medium transition-colors relative ${
                                activeTab === tab.id
                                    ? 'text-blue-500 dark:text-cyan-400'
                                    : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-300'
                            }`}
                        >
                            {tab.label}
                            {activeTab === tab.id && (
                                <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-blue-500 dark:bg-cyan-400" />
                            )}
                        </button>
                    ))}
                </div>

                {/* Tab content */}
                <div className="flex-1 overflow-y-auto min-h-0">
                    {/* Info Tab */}
                    {activeTab === 'info' && (
                        <div className="p-5 space-y-4">
                            {/* Logo + version + tagline */}
                            <div className="text-center">
                                <div className="w-16 h-16 mx-auto mb-2 bg-gray-100 dark:bg-gray-800 rounded-2xl shadow-sm flex items-center justify-center p-1.5 border border-gray-200 dark:border-gray-700">
                                    <img
                                        src="/icons/AeroFTP_simbol_color_512x512.png"
                                        alt="AeroFTP"
                                        className="w-full h-full object-contain"
                                    />
                                </div>
                                <p className="text-xs text-gray-500 font-mono mb-1">v{appVersion}</p>
                                <p className="text-sm text-gray-500 dark:text-gray-400">
                                    {t('about.tagline')}
                                </p>
                            </div>

                            {/* Features list */}
                            <div className="grid grid-cols-3 gap-x-2 gap-y-1.5 text-xs text-gray-500 dark:text-gray-400 py-2">
                                <div className="font-mono">{t('about.features.rustEngine')}</div>
                                <div className="font-mono">{t('about.features.monacoEditor')}</div>
                                <div className="font-mono">{t('about.features.ptyTerminal')}</div>
                                <div className="font-mono">{t('about.features.aiAgent')}</div>
                                <div className="font-mono">{t('about.features.ftpsSecure')}</div>
                                <div className="font-mono">{t('about.features.fileSync')}</div>
                                <div className="font-mono">{t('about.features.aeroCloud')}</div>
                                <div className="font-mono">{t('about.features.mediaPlayer')}</div>
                                <div className="font-mono">{t('about.features.imagePreview')}</div>
                            </div>

                            {/* Protocols & Providers */}
                            <div className="text-center py-2 border-t border-gray-200 dark:border-gray-800">
                                <p className="text-[11px] text-gray-500 font-mono">
                                    FTP / FTPS / SFTP / WebDAV / S3 / GitHub
                                </p>
                                <p className="text-[11px] text-gray-500 font-mono">
                                    Google Drive / Dropbox / OneDrive / MEGA / Box / Filen
                                </p>
                                <p className="text-[10px] text-gray-400 dark:text-gray-600 font-mono mt-1">
                                    25 protocols &middot; 47 languages &middot; AES-256 archives
                                </p>
                            </div>

                            {/* License */}
                            <div className="text-center py-2 border-t border-gray-200 dark:border-gray-800">
                                <p className="text-xs text-gray-500 dark:text-gray-400 font-mono">{t('about.license')}</p>
                                <button
                                    onClick={() => openUrl('https://www.gnu.org/licenses/gpl-3.0.html')}
                                    className="text-[11px] text-blue-500 dark:text-cyan-500 hover:text-blue-400 dark:hover:text-cyan-400 transition-colors font-mono mt-0.5 inline-block"
                                >
                                    GNU General Public License v3.0
                                </button>
                            </div>

                            {/* Credits */}
                            <div className="text-center pt-2 border-t border-gray-200 dark:border-gray-800 space-y-1">
                                <p className="text-xs text-gray-500 flex items-center justify-center gap-1 font-mono">
                                    {t('about.madeWith')} <Heart size={12} className="text-red-500" /> by AxpDev
                                </p>
                                <p className="text-[10px] text-gray-400 dark:text-gray-600 font-mono">
                                    {t('about.aiCredits')}
                                </p>
                                <p className="text-[10px] text-gray-400 dark:text-gray-700 font-mono">
                                    {t('about.copyright')}
                                </p>
                            </div>
                        </div>
                    )}

                    {/* Technical Tab */}
                    {activeTab === 'technical' && (
                        <div className="p-5 space-y-4">
                            {/* Build info */}
                            <div>
                                <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2">{t('about.buildInfo')}</h3>
                                <div className="bg-gray-50 dark:bg-gray-800/50 rounded-lg px-3 py-1">
                                    <InfoRow label="Tauri" value={systemInfo?.tauri_version ?? '...'} />
                                    <InfoRow label="Rust" value={systemInfo?.rust_version ?? '...'} />
                                    {FRONTEND_DEPS.map(dep => (
                                        <InfoRow key={dep.name} label={dep.name} value={dep.version} />
                                    ))}
                                </div>
                            </div>

                            {/* System Details */}
                            <div>
                                <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2">{t('about.systemDetails')}</h3>
                                <div className="bg-gray-50 dark:bg-gray-800/50 rounded-lg px-3 py-1">
                                    <InfoRow label={t('about.operatingSystem')} value={systemInfo?.os ?? '...'} />
                                    <InfoRow label={t('about.architecture')} value={systemInfo?.arch ?? '...'} />
                                    <InfoRow label={t('about.keyringBackend')} value={systemInfo?.keyring_backend ?? '...'} />
                                    <InfoRow label={t('about.configDir')} value={systemInfo?.config_dir ?? '...'} />
                                    <InfoRow label={t('about.vaultStatus')} value={
                                        systemInfo ? (systemInfo.vault_exists ? t('about.active') : t('about.inactive')) : '...'
                                    } />
                                    <InfoRow label={t('about.knownHosts')} value={
                                        systemInfo ? (systemInfo.known_hosts_exists ? t('about.found') : t('about.notFound')) : '...'
                                    } />
                                </div>
                            </div>

                            {/* Linked Libraries */}
                            <div>
                                <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2">{t('about.linkedLibraries')}</h3>
                                <div className="bg-gray-50 dark:bg-gray-800/50 rounded-lg px-3 py-1">
                                    {KEY_DEPENDENCY_LABELS.map(dep => (
                                        <InfoRow key={dep.name} label={dep.name} value={
                                            <span>{systemInfo?.dep_versions?.[dep.name] ?? '...'} <span className="text-gray-400 dark:text-gray-600">({dep.description})</span></span>
                                        } />
                                    ))}
                                </div>
                            </div>

                        </div>
                    )}

                    {/* Support Tab */}
                    {activeTab === 'support' && (
                        <div className="p-5 space-y-4">
                            {/* Links */}
                            <div className="flex justify-center gap-3">
                                <button
                                    onClick={() => openUrl('https://github.com/axpdev-lab/aeroftp')}
                                    className="flex items-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-sm text-gray-600 dark:text-gray-300"
                                >
                                    <Github size={16} />
                                    {t('about.github')}
                                </button>
                                <button
                                    onClick={() => openUrl('https://sourceforge.net/projects/aeroftp/?pk_campaign=badge&pk_source=vendor')}
                                    className="flex items-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-sm text-gray-600 dark:text-gray-300"
                                >
                                    <SourceForge size={16} />
                                    SourceForge
                                </button>
                                <button
                                    onClick={() => openUrl('mailto:aeroftp@axpdev.it')}
                                    className="flex items-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-sm text-gray-600 dark:text-gray-300"
                                >
                                    <Mail size={16} />
                                    {t('about.contact')}
                                </button>
                            </div>

                            {/* Reviews */}
                            <div className="flex justify-center">
                                <button
                                    onClick={() => openUrl('https://sourceforge.net/software/product/AeroFTP/')}
                                    className="inline-flex items-center gap-1.5 text-xs text-blue-500 dark:text-cyan-500 hover:text-blue-400 dark:hover:text-cyan-400 transition-colors font-mono"
                                >
                                    <ExternalLink size={12} />
                                    {t('about.sourceforgeReviews')}
                                </button>
                            </div>

                            {/* Website & Docs */}
                            <div className="flex justify-center gap-4">
                                <button
                                    onClick={() => openUrl('https://aeroftp.app')}
                                    className="inline-flex items-center gap-1.5 text-xs text-blue-500 dark:text-cyan-500 hover:text-blue-400 dark:hover:text-cyan-400 transition-colors font-mono"
                                >
                                    <ExternalLink size={12} />
                                    aeroftp.app
                                </button>
                                <button
                                    onClick={() => openUrl('https://docs.aeroftp.app')}
                                    className="inline-flex items-center gap-1.5 text-xs text-blue-500 dark:text-cyan-500 hover:text-blue-400 dark:hover:text-cyan-400 transition-colors font-mono"
                                >
                                    <ExternalLink size={12} />
                                    docs.aeroftp.app
                                </button>
                            </div>

                            {/* Support & Report */}
                            <div className="border-t border-gray-200 dark:border-gray-800 pt-4 space-y-2">
                                <p className="text-xs text-gray-500 dark:text-gray-400 text-center">
                                    {t('about.supportDesc')}
                                </p>
                                <div className="flex justify-center gap-3">
                                    <button
                                        onClick={() => openUrl('https://www.aeroftp.app/page/report-issue')}
                                        className="flex items-center gap-1.5 px-3 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-xs text-gray-600 dark:text-gray-300"
                                    >
                                        <ExternalLink size={12} />
                                        {t('about.reportIssue')}
                                    </button>
                                    <button
                                        onClick={() => openUrl('https://docs.aeroftp.app')}
                                        className="flex items-center gap-1.5 px-3 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-xs text-gray-600 dark:text-gray-300"
                                    >
                                        <ExternalLink size={12} />
                                        {t('about.documentation')}
                                    </button>
                                </div>
                            </div>
                        </div>
                    )}
                </div>

                {/* Bottom bar with copy button (visible on technical tab) */}
                {activeTab === 'technical' && (
                    <div className="shrink-0 border-t border-gray-200 dark:border-gray-700 px-4 py-3 flex justify-between items-center">
                        <button
                            onClick={copyTechnicalInfo}
                            className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-xs font-mono transition-colors ${
                                copied
                                    ? 'bg-green-100 dark:bg-green-500/20 text-green-600 dark:text-green-400 border border-green-300 dark:border-green-500/30'
                                    : 'bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400 border border-gray-300 dark:border-gray-700'
                            }`}
                        >
                            {copied ? <Check size={14} /> : <Copy size={14} />}
                            {copied ? t('toast.clipboardCopied') : t('about.copyToClipboard')}
                        </button>
                        <button
                            onClick={onClose}
                            className="px-4 py-1.5 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg text-xs text-gray-600 dark:text-gray-300 transition-colors"
                        >
                            {t('common.ok')}
                        </button>
                    </div>
                )}
            </div>

        </div>
    );
};

export default AboutDialog;
