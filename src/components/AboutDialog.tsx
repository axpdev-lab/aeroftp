import * as React from 'react';
import { useState, useEffect } from 'react';
import { X, Github, Heart, Cpu, Globe, Mail, Copy, Check, ChevronDown, ChevronUp, Wallet } from 'lucide-react';
import { getVersion } from '@tauri-apps/api/app';
import { useTranslation } from '../i18n';
import { openUrl } from '../utils/openUrl';

interface AboutDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

// Crypto addresses for donations
const CRYPTO_ADDRESSES = {
    btc: {
        name: 'Bitcoin',
        symbol: 'BTC',
        address: 'bc1qdxur90s5j4s55rwe9rc9n95fau4rg3tfatfhkn',
        icon: '₿',
        color: 'from-orange-500 to-yellow-500',
        bgColor: 'bg-orange-500/20',
    },
    eth: {
        name: 'Ethereum / EVM',
        symbol: 'ETH',
        address: '0x08F9D9C41E833539Fd733e19119A89f0664c3AeE',
        icon: 'Ξ',
        color: 'from-blue-400 to-purple-500',
        bgColor: 'bg-blue-500/20',
    },
    sol: {
        name: 'Solana',
        symbol: 'SOL',
        address: '25A8sBNqzbR9rvrd3qyYwBkwirEh1pUiegUG6CrswHrd',
        icon: '◎',
        color: 'from-purple-500 to-green-400',
        bgColor: 'bg-purple-500/20',
    },
    ltc: {
        name: 'Litecoin',
        symbol: 'LTC',
        address: 'LTk8iRvUqAtYyer8SPAkEAakpPXxfFY1D1',
        icon: 'Ł',
        color: 'from-gray-400 to-blue-400',
        bgColor: 'bg-gray-500/20',
    },
};

// Crypto Donate Panel Component
const CryptoDonatePanel: React.FC<{ onClose?: () => void }> = () => {
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
                    className={`rounded-lg border border-gray-700 overflow-hidden transition-all duration-200 ${expandedChain === key ? 'bg-gray-800/80' : 'bg-gray-800/40 hover:bg-gray-800/60'
                        }`}
                >
                    {/* Chain Header */}
                    <button
                        onClick={() => setExpandedChain(expandedChain === key ? null : key)}
                        className="w-full flex items-center justify-between px-3 py-2 text-left"
                    >
                        <div className="flex items-center gap-3">
                            <span className={`w-8 h-8 rounded-lg bg-gradient-to-br ${chain.color} flex items-center justify-center text-white font-bold text-sm`}>
                                {chain.icon}
                            </span>
                            <div>
                                <div className="text-sm font-medium text-gray-200">{chain.name}</div>
                                <div className="text-xs text-gray-500">{chain.symbol}</div>
                            </div>
                        </div>
                        {expandedChain === key ? <ChevronUp size={16} className="text-gray-500" /> : <ChevronDown size={16} className="text-gray-500" />}
                    </button>

                    {/* Expanded Address */}
                    {expandedChain === key && (
                        <div className="px-3 pb-3 space-y-2">
                            <div className="flex items-center gap-2 p-2 bg-gray-900 rounded-lg border border-gray-700">
                                <code className="flex-1 text-xs text-green-400 font-mono break-all select-all">
                                    {chain.address}
                                </code>
                                <button
                                    onClick={() => copyAddress(key, chain.address)}
                                    className={`p-1.5 rounded transition-colors ${copiedChain === key
                                        ? 'bg-green-500/20 text-green-400'
                                        : 'bg-gray-700 hover:bg-gray-600 text-gray-400'
                                        }`}
                                    title={t('common.copy')}
                                >
                                    {copiedChain === key ? <Check size={14} /> : <Copy size={14} />}
                                </button>
                            </div>
                            {copiedChain === key && (
                                <div className="text-xs text-green-400 text-center animate-pulse">
                                    ✓ {t('toast.clipboardCopied')}
                                </div>
                            )}
                        </div>
                    )}
                </div>
            ))}

            {/* Cyber Footer */}
            <div className="text-center pt-2">
                <p className="text-[10px] text-gray-600 font-mono">
                    // {t('support.footer')}
                </p>
            </div>
        </div>
    );
};

export const AboutDialog: React.FC<AboutDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [showDonatePanel, setShowDonatePanel] = useState(false);
    const [appVersion, setAppVersion] = useState('0.0.0');

    useEffect(() => {
        getVersion().then(setAppVersion).catch(() => setAppVersion('0.8.3'));
    }, []);

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
            {/* Backdrop */}
            <div
                className="absolute inset-0 bg-black/60 backdrop-blur-sm"
                onClick={onClose}
            />

            {/* Dialog */}
            <div className="relative bg-gray-900 border border-gray-700 rounded-2xl shadow-2xl w-full max-w-md overflow-hidden animate-scale-in">
                {/* Cyber Header */}
                <div className="bg-gradient-to-br from-cyan-600 via-blue-600 to-purple-700 p-6 text-white text-center relative overflow-hidden">
                    {/* Grid overlay for cyber effect */}
                    <div className="absolute inset-0 opacity-10" style={{
                        backgroundImage: 'linear-gradient(rgba(255,255,255,.1) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,.1) 1px, transparent 1px)',
                        backgroundSize: '20px 20px'
                    }} />

                    {/* Close button */}
                    <button
                        onClick={onClose}
                        className="absolute top-3 right-3 p-1.5 rounded-full bg-white/20 hover:bg-white/30 transition-colors"
                        title={t('common.close')}
                    >
                        <X size={16} />
                    </button>

                    {/* Logo */}
                    <div className="w-20 h-20 mx-auto mb-4 bg-white/10 backdrop-blur-sm rounded-2xl shadow-lg flex items-center justify-center p-2 border border-white/20">
                        <img
                            src="/icons/AeroFTP_simbol_color_512x512.png"
                            alt="AeroFTP"
                            className="w-full h-full object-contain"
                        />
                    </div>

                    <h1 className="text-2xl font-bold font-mono">AeroFTP</h1>
                    <p className="text-cyan-200 text-sm mt-1 font-mono">{t('common.version')} {appVersion}</p>
                </div>

                {/* Content */}
                <div className="p-5 space-y-4">
                    <p className="text-center text-gray-400 text-sm">
                        {'>'} {t('about.tagline')}
                    </p>

                    {/* Features Grid - 3 columns with all 9 features */}
                    <div className="grid grid-cols-3 gap-1.5 text-xs text-gray-500 py-2">
                        <div className="flex items-center gap-1.5 font-mono">⚡ {t('about.features.rustEngine')}</div>
                        <div className="flex items-center gap-1.5 font-mono">📝 {t('about.features.monacoEditor')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🖥️ {t('about.features.ptyTerminal')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🤖 {t('about.features.aiAgent')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🔒 {t('about.features.ftpsSecure')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🔄 {t('about.features.fileSync')}</div>
                        <div className="flex items-center gap-1.5 font-mono">☁️ {t('about.features.aeroCloud')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🎵 {t('about.features.mediaPlayer')}</div>
                        <div className="flex items-center gap-1.5 font-mono">🖼️ {t('about.features.imagePreview')}</div>
                    </div>

                    {/* Tech Stack */}
                    <div className="flex justify-center gap-4 py-2 border-t border-gray-800">
                        <div className="flex items-center gap-2 text-xs text-gray-600">
                            <Cpu size={14} />
                            <span className="font-mono">Rust + Tauri</span>
                        </div>
                        <div className="flex items-center gap-2 text-xs text-gray-600">
                            <Globe size={14} />
                            <span className="font-mono">React + TS</span>
                        </div>
                    </div>

                    {/* Links */}
                    <div className="flex justify-center gap-3">
                        <button
                            onClick={() => openUrl('https://github.com/axpnet/aeroftp')}
                            className="flex items-center gap-2 px-4 py-2 bg-gray-800 hover:bg-gray-700 border border-gray-700 rounded-lg transition-colors text-sm text-gray-300"
                        >
                            <Github size={16} />
                            {t('about.github')}
                        </button>
                        <button
                            onClick={() => openUrl('mailto:aeroftp@axpdev.it')}
                            className="flex items-center gap-2 px-4 py-2 bg-gray-800 hover:bg-gray-700 border border-gray-700 rounded-lg transition-colors text-sm text-gray-300"
                        >
                            <Mail size={16} />
                            {t('about.contact')}
                        </button>
                    </div>

                    {/* Crypto Donations Section */}
                    <div className="border-t border-gray-800 pt-4">
                        <button
                            onClick={() => setShowDonatePanel(!showDonatePanel)}
                            className={`w-full flex items-center justify-center gap-2 px-4 py-3 rounded-xl transition-all duration-300 ${showDonatePanel
                                ? 'bg-gradient-to-r from-cyan-600 to-purple-600 text-white'
                                : 'bg-gradient-to-r from-gray-800 to-gray-700 hover:from-cyan-900 hover:to-purple-900 text-gray-300 border border-gray-700'
                                }`}
                        >
                            <Wallet size={18} />
                            <span className="font-mono text-sm">
                                {showDonatePanel ? `// ${t('about.donateWith')}` : `💰 ${t('about.supportDev')}`}
                            </span>
                            {showDonatePanel ? <ChevronUp size={16} /> : <ChevronDown size={16} />}
                        </button>

                        {showDonatePanel && (
                            <div className="mt-3 animate-slide-down">
                                <CryptoDonatePanel />
                            </div>
                        )}
                    </div>

                    {/* Credits */}
                    <div className="text-center pt-3 border-t border-gray-800">
                        <p className="text-xs text-gray-500 flex items-center justify-center gap-1 font-mono">
                            {'>'} {t('about.madeWith')} <Heart size={12} className="text-red-500" /> by AxpDev
                        </p>
                        <p className="text-[10px] text-gray-600 mt-1 font-mono">
                            // {t('about.aiCredits')}
                        </p>
                        <p className="text-[10px] text-gray-700 mt-1 font-mono">
                            {t('about.copyright')}
                        </p>
                    </div>
                </div>
            </div>

            <style>{`
                @keyframes slide-down {
                    from { opacity: 0; transform: translateY(-10px); }
                    to { opacity: 1; transform: translateY(0); }
                }
                .animate-slide-down {
                    animation: slide-down 0.3s ease-out;
                }
            `}</style>
        </div>
    );
};

export default AboutDialog;
