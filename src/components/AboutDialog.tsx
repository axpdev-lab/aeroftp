import * as React from 'react';
import { useState, useEffect, useMemo } from 'react';
import { X, Github, Mail, Copy, Check, ChevronDown, ChevronUp, Wallet, Heart, ExternalLink } from 'lucide-react';
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

// Official Crypto SVG Icons (matching SupportDialog)
const BitcoinIcon = () => (
    <svg viewBox="0 0 32 32" className="w-5 h-5" fill="currentColor">
        <path fill="#f7931a" d="M16 0c8.837 0 16 7.163 16 16s-7.163 16-16 16S0 24.837 0 16 7.163 0 16 0z"/>
        <path fill="#fff" d="M22.5 14.1c.3-2.1-1.3-3.2-3.4-3.9l.7-2.8-1.7-.4-.7 2.7c-.4-.1-.9-.2-1.4-.3l.7-2.7-1.7-.4-.7 2.8c-.3-.1-.7-.2-1-.3l-2.4-.6-.5 1.8s1.3.3 1.2.3c.7.2.8.6.8 1l-.8 3.3s.1 0 .2.1l-.2-.1-1.1 4.5c-.1.2-.3.5-.8.4 0 0-1.2-.3-1.2-.3l-.8 2 2.2.6 1.2.3-.7 2.8 1.7.4.7-2.8c.5.1 .9.2 1.4.3l-.7 2.8 1.7.4.7-2.8c2.9.5 5.1.3 6-2.3.7-2.1-.1-3.3-1.5-4.1 1.1-.2 1.9-.9 2.1-2.4zm-3.8 5.3c-.5 2.1-4 1-5.1.7l.9-3.7c1.2.3 4.7.8 4.2 3zm.5-5.4c-.5 1.9-3.4 1-4.3.7l.8-3.3c1 .2 4 .7 3.5 2.6z"/>
    </svg>
);
const EthereumIcon = () => (
    <svg viewBox="0 0 32 32" className="w-5 h-5" fill="currentColor">
        <path fill="#627eea" d="M16 0c8.837 0 16 7.163 16 16s-7.163 16-16 16S0 24.837 0 16 7.163 0 16 0z"/>
        <path fill="#fff" fillOpacity=".6" d="M16.5 4v8.87l7.5 3.35z"/>
        <path fill="#fff" d="M16.5 4L9 16.22l7.5-3.35z"/>
        <path fill="#fff" fillOpacity=".6" d="M16.5 21.97v6.03L24 17.62z"/>
        <path fill="#fff" d="M16.5 28V21.97L9 17.62z"/>
        <path fill="#fff" fillOpacity=".2" d="M16.5 20.57l7.5-4.35-7.5-3.35z"/>
        <path fill="#fff" fillOpacity=".6" d="M9 16.22l7.5 4.35v-7.7z"/>
    </svg>
);
const SolanaIcon = () => (
    <svg viewBox="0 0 32 32" className="w-5 h-5" fill="currentColor">
        <defs>
            <linearGradient id="about-sol-grad" x1="0%" y1="0%" x2="100%" y2="100%">
                <stop offset="0%" stopColor="#9945ff"/>
                <stop offset="100%" stopColor="#14f195"/>
            </linearGradient>
        </defs>
        <circle cx="16" cy="16" r="16" fill="url(#about-sol-grad)"/>
        <path fill="#fff" d="M10.5 19.5c.2-.2.4-.3.7-.3h12.1c.4 0 .7.5.3.8l-2.4 2.4c-.2.2-.4.3-.7.3H8.4c-.4 0-.7-.5-.3-.8l2.4-2.4z"/>
        <path fill="#fff" d="M10.5 9.3c.2-.2.4-.3.7-.3h12.1c.4 0 .7.5.3.8l-2.4 2.4c-.2.2-.4.3-.7.3H8.4c-.4 0-.7-.5-.3-.8l2.4-2.4z"/>
        <path fill="#fff" d="M21.5 14.4c-.2-.2-.4-.3-.7-.3H8.7c-.4 0-.7.5-.3.8l2.4 2.4c.2.2.4.3.7.3h12.1c.4 0 .7-.5.3-.8l-2.4-2.4z"/>
    </svg>
);
const LitecoinIcon = () => (
    <svg viewBox="0 0 508.96 508.96" className="w-5 h-5">
        <circle fill="#fff" cx="254.48" cy="254.48" r="226.94"/>
        <path fill="#345d9d" d="M256.38,2C115.84,2,1.9,116,1.9,256.52S115.84,511,256.38,511,510.87,397.07,510.87,256.52h0C511.27,116.38,398,2.45,257.87,2h-1.49Zm4.32,263.11-26.5,89.34H375.92a7.15,7.15,0,0,1,7.4,6.89h0v2.34L371,406.25a9.18,9.18,0,0,1-9.24,6.78H144.86l36.35-123.85L140.54,301.5l9.25-28.34,40.66-12.33L241.6,87.07a9.3,9.3,0,0,1,9.24-6.78h54.84a7.15,7.15,0,0,1,7.39,6.9h0v2.35L269.94,236.19l40.67-12.33L302,253.44Z" transform="translate(-1.9 -2.04)"/>
    </svg>
);

// Crypto addresses for donations
const CRYPTO_ADDRESSES = {
    btc: {
        name: 'Bitcoin',
        symbol: 'BTC',
        address: 'bc1qdxur90s5j4s55rwe9rc9n95fau4rg3tfatfhkn',
        Icon: BitcoinIcon,
    },
    eth: {
        name: 'Ethereum / EVM',
        symbol: 'ETH',
        address: '0x08F9D9C41E833539Fd733e19119A89f0664c3AeE',
        Icon: EthereumIcon,
    },
    sol: {
        name: 'Solana',
        symbol: 'SOL',
        address: '25A8sBNqzbR9rvrd3qyYwBkwirEh1pUiegUG6CrswHrd',
        Icon: SolanaIcon,
    },
    ltc: {
        name: 'Litecoin',
        symbol: 'LTC',
        address: 'LTk8iRvUqAtYyer8SPAkEAakpPXxfFY1D1',
        Icon: LitecoinIcon,
    },
};

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

// Crypto Donate Panel Component
const CryptoDonatePanel: React.FC = () => {
    const t = useTranslation();
    const [copiedChain, setCopiedChain] = useState<string | null>(null);
    const [expandedChain, setExpandedChain] = useState<string | null>(null);

    const copyAddress = (chain: string, address: string) => {
        navigator.clipboard.writeText(address);
        setCopiedChain(chain);
        setTimeout(() => setCopiedChain(null), 2000);
    };

    return (
        <div className="space-y-2">
            {Object.entries(CRYPTO_ADDRESSES).map(([key, chain]) => (
                <div
                    key={key}
                    className={`rounded-lg border border-gray-200 dark:border-gray-700 overflow-hidden transition-all duration-200 ${expandedChain === key ? 'bg-gray-100 dark:bg-gray-800/80' : 'bg-gray-50 dark:bg-gray-800/40 hover:bg-gray-100 dark:hover:bg-gray-800/60'}`}
                >
                    <button
                        onClick={() => setExpandedChain(expandedChain === key ? null : key)}
                        className="w-full flex items-center justify-between px-3 py-2 text-left"
                    >
                        <div className="flex items-center gap-3">
                            <span className="w-8 h-8 rounded-lg flex items-center justify-center">
                                <chain.Icon />
                            </span>
                            <div>
                                <div className="text-sm font-medium text-gray-700 dark:text-gray-200">{chain.name}</div>
                                <div className="text-xs text-gray-500">{chain.symbol}</div>
                            </div>
                        </div>
                        {expandedChain === key ? <ChevronUp size={16} className="text-gray-500" /> : <ChevronDown size={16} className="text-gray-500" />}
                    </button>

                    {expandedChain === key && (
                        <div className="px-3 pb-3 space-y-2">
                            <div className="flex items-center gap-2 p-2 bg-white dark:bg-gray-900 rounded-lg border border-gray-200 dark:border-gray-700">
                                <code className="flex-1 text-xs text-green-600 dark:text-green-400 font-mono break-all select-all">
                                    {chain.address}
                                </code>
                                <button
                                    onClick={() => copyAddress(key, chain.address)}
                                    className={`p-1.5 rounded transition-colors ${copiedChain === key
                                        ? 'bg-green-100 dark:bg-green-500/20 text-green-600 dark:text-green-400'
                                        : 'bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-500 dark:text-gray-400'
                                        }`}
                                    title={t('common.copy')}
                                >
                                    {copiedChain === key ? <Check size={14} /> : <Copy size={14} />}
                                </button>
                            </div>
                            {copiedChain === key && (
                                <div className="text-xs text-green-400 text-center animate-pulse">
                                    {t('toast.clipboardCopied')}
                                </div>
                            )}
                        </div>
                    )}
                </div>
            ))}

            <div className="text-center pt-2">
                <p className="text-[10px] text-gray-400 dark:text-gray-600 font-mono">
                    {t('support.footer')}
                </p>
            </div>
        </div>
    );
};

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
    const [showDonatePanel, setShowDonatePanel] = useState(false);
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
        setShowDonatePanel(false);
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

            <div className="relative bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-lg overflow-hidden flex flex-col" style={{ maxHeight: '85vh' }}>
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
                                    FTP / FTPS / SFTP / WebDAV / S3
                                </p>
                                <p className="text-[11px] text-gray-500 font-mono">
                                    Google Drive / Dropbox / OneDrive / MEGA / Box / Filen
                                </p>
                                <p className="text-[10px] text-gray-400 dark:text-gray-600 font-mono mt-1">
                                    14 protocols &middot; 47 languages &middot; AES-256 archives
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
                                    onClick={() => openUrl('https://github.com/axpnet/aeroftp')}
                                    className="flex items-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-sm text-gray-600 dark:text-gray-300"
                                >
                                    <Github size={16} />
                                    {t('about.github')}
                                </button>
                                <button
                                    onClick={() => openUrl('mailto:aeroftp@axpdev.it')}
                                    className="flex items-center gap-2 px-4 py-2 bg-gray-100 dark:bg-gray-800 hover:bg-gray-200 dark:hover:bg-gray-700 border border-gray-300 dark:border-gray-700 rounded-lg transition-colors text-sm text-gray-600 dark:text-gray-300"
                                >
                                    <Mail size={16} />
                                    {t('about.contact')}
                                </button>
                            </div>

                            {/* Website */}
                            <div className="text-center">
                                <button
                                    onClick={() => openUrl('https://github.com/axpnet/aeroftp')}
                                    className="inline-flex items-center gap-1.5 text-xs text-blue-500 dark:text-cyan-500 hover:text-blue-400 dark:hover:text-cyan-400 transition-colors font-mono"
                                >
                                    <ExternalLink size={12} />
                                    github.com/axpnet/aeroftp
                                </button>
                            </div>

                            {/* Crypto Donations */}
                            <div className="border-t border-gray-200 dark:border-gray-800 pt-4">
                                <button
                                    onClick={() => setShowDonatePanel(!showDonatePanel)}
                                    className={`w-full flex items-center justify-center gap-2 px-4 py-3 rounded-xl transition-all duration-300 ${showDonatePanel
                                        ? 'bg-blue-500 dark:bg-gradient-to-r dark:from-cyan-600 dark:to-purple-600 text-white'
                                        : 'bg-gray-100 dark:bg-gradient-to-r dark:from-gray-800 dark:to-gray-700 hover:bg-gray-200 dark:hover:from-cyan-900 dark:hover:to-purple-900 text-gray-600 dark:text-gray-300 border border-gray-300 dark:border-gray-700'
                                        }`}
                                >
                                    <Wallet size={18} />
                                    <span className="font-mono text-sm">
                                        {t(showDonatePanel ? 'about.donateWith' : 'about.supportDev')}
                                    </span>
                                    {showDonatePanel ? <ChevronUp size={16} /> : <ChevronDown size={16} />}
                                </button>

                                {showDonatePanel && (
                                    <div className="mt-3 animate-slide-down">
                                        <CryptoDonatePanel />
                                    </div>
                                )}
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
