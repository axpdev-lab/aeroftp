/**
 * SupportDialog Component
 * Donation dialog with fiat (PayPal, GitHub Sponsors, Buy Me a Coffee) and crypto options with QR codes
 */

import * as React from 'react';
import { useState } from 'react';
import { X, Heart, Copy, Check, ExternalLink, CreditCard } from 'lucide-react';
import { QRCodeSVG } from 'qrcode.react';
import { useTranslation } from '../i18n';
import { openUrl } from '../utils/openUrl';

interface SupportDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

// Official GitHub SVG Icon
const GitHubIcon = () => (
    <svg viewBox="0 0 24 24" className="w-6 h-6" fill="currentColor">
        <path d="M12 0C5.374 0 0 5.373 0 12c0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23A11.509 11.509 0 0 1 12 5.803c1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576C20.566 21.797 24 17.3 24 12c0-6.627-5.373-12-12-12z"/>
    </svg>
);

// Coffee Icon (stylized) - black in light mode, white in dark mode
const CoffeeIcon = () => (
    <svg viewBox="-5 0 32 32" className="w-6 h-6 fill-gray-900 dark:fill-white">
        <path d="M12.406 14.75c-0.094-2.094-0.219-3.219-1.469-4.594-1.594-1.781-2.188-3.5-0.875-6.156 0.344 1.781 0.469 3.375 1.719 4.344s2.281 3.594 0.625 6.406zM10.063 14.75c-0.063-1.125-0.125-1.688-0.813-2.469-0.844-0.938-1.188-1.844-0.469-3.281 0.188 0.969 0.219 1.813 0.906 2.313s1.281 1.938 0.375 3.438zM15.719 24.625h5.688c0.344 0 0.469 0.25 0.25 0.531 0 0-2.219 2.844-5.281 2.844h-10.969s-5.281-2.844-5.281-2.844c-0.219-0.281-0.125-0.531 0.219-0.531h5.625c-0.781-0.406-1.938-2.188-1.938-4.406v-4.688h13.688v0.375c0.438-0.375 0.969-0.563 1.531-0.563 0.781 0 2.25 0.813 2.25 2.219 0 2.031-1.344 2.781-2.125 3.313 0 0-1.469 1.156-2.5 2.5-0.344 0.594-0.75 1.063-1.156 1.25zM19.25 16.188c-0.5 0-1.125 0.219-1.531 1.219v2.594c0 0.344-0.031 0.75-0.094 1.094 0.688-0.688 1.5-1.156 1.5-1.156 0.5-0.344 1.5-1 1.5-2.281 0.031-0.906-0.813-1.469-1.375-1.469zM6.406 16.563h-0.875v1.281h0.875v-1.281zM6.406 18.594h-0.875v2.094s0.25 2.813 2.031 3.656c-1.094-1.281-1.156-2.75-1.156-3.656v-2.094z" />
    </svg>
);

// Crypto Wallet Icon - for crypto donation section
const CryptoWalletIcon = () => (
    <svg viewBox="0 0 24 24" className="w-5 h-5 fill-gray-500 dark:fill-gray-400">
        <path fillRule="evenodd" clipRule="evenodd" d="M16 3.5C15.1716 3.5 14.5 4.17157 14.5 5C14.5 5.65311 14.9174 6.20873 15.5 6.41465V7.5H7C6.17157 7.5 5.5 8.17157 5.5 9V15C5.5 15.8284 6.17157 16.5 7 16.5H8.5V17.5854C7.9174 17.7913 7.5 18.3469 7.5 19C7.5 19.8284 8.17157 20.5 9 20.5C9.82843 20.5 10.5 19.8284 10.5 19C10.5 18.3469 10.0826 17.7913 9.5 17.5854V16.5H17C17.8284 16.5 18.5 15.8284 18.5 15V13.4146C19.0826 13.2087 19.5 12.6531 19.5 12C19.5 11.3469 19.0826 10.7913 18.5 10.5854V9C18.5 8.17157 17.8284 7.5 17 7.5H16.5V6.41465C17.0826 6.20873 17.5 5.65311 17.5 5C17.5 4.17157 16.8284 3.5 16 3.5ZM15.5 5C15.5 4.72386 15.7239 4.5 16 4.5C16.2761 4.5 16.5 4.72386 16.5 5C16.5 5.27614 16.2761 5.5 16 5.5C15.7239 5.5 15.5 5.27614 15.5 5ZM17.5 10.5V9C17.5 8.72386 17.2761 8.5 17 8.5H7C6.72386 8.5 6.5 8.72386 6.5 9V15C6.5 15.2761 6.72386 15.5 7 15.5H17C17.2761 15.5 17.5 15.2761 17.5 15V13.5H14C13.1716 13.5 12.5 12.8284 12.5 12C12.5 11.1716 13.1716 10.5 14 10.5H17.5ZM9 18.5C8.72386 18.5 8.5 18.7239 8.5 19C8.5 19.2761 8.72386 19.5 9 19.5C9.27614 19.5 9.5 19.2761 9.5 19C9.5 18.7239 9.27614 18.5 9 18.5ZM13.5 12C13.5 11.7239 13.7239 11.5 14 11.5H18C18.2761 11.5 18.5 11.7239 18.5 12C18.5 12.2761 18.2761 12.5 18 12.5H14C13.7239 12.5 13.5 12.2761 13.5 12Z" />
    </svg>
);

// Payment links - simplified with transparent backgrounds
const PAYMENT_LINKS = {
    github: {
        name: 'GitHub Sponsors',
        url: 'https://github.com/sponsors/axpnet',
        Icon: GitHubIcon,
        textColor: 'text-gray-700 dark:text-gray-300',
    },
    buymeacoffee: {
        name: 'Buy Me a Coffee',
        url: 'https://buymeacoffee.com/axpnet',
        Icon: CoffeeIcon,
        textColor: 'text-gray-900 dark:text-white',
    },
};

// Official Crypto SVG Icons
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
            <linearGradient id="sol-grad" x1="0%" y1="0%" x2="100%" y2="100%">
                <stop offset="0%" stopColor="#9945ff"/>
                <stop offset="100%" stopColor="#14f195"/>
            </linearGradient>
        </defs>
        <circle cx="16" cy="16" r="16" fill="url(#sol-grad)"/>
        <path fill="#fff" d="M10.5 19.5c.2-.2.4-.3.7-.3h12.1c.4 0 .7.5.3.8l-2.4 2.4c-.2.2-.4.3-.7.3H8.4c-.4 0-.7-.5-.3-.8l2.4-2.4z"/>
        <path fill="#fff" d="M10.5 9.3c.2-.2.4-.3.7-.3h12.1c.4 0 .7.5.3.8l-2.4 2.4c-.2.2-.4.3-.7.3H8.4c-.4 0-.7-.5-.3-.8l2.4-2.4z"/>
        <path fill="#fff" d="M21.5 14.4c-.2-.2-.4-.3-.7-.3H8.7c-.4 0-.7.5-.3.8l2.4 2.4c.2.2.4.3.7.3h12.1c.4 0 .7-.5.3-.8l-2.4-2.4z"/>
    </svg>
);

// Official Litecoin SVG Icon
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

export const SupportDialog: React.FC<SupportDialogProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [selectedCrypto, setSelectedCrypto] = useState<string | null>(null);
    const [copiedAddress, setCopiedAddress] = useState<string | null>(null);

    const copyToClipboard = (key: string, address: string) => {
        navigator.clipboard.writeText(address);
        setCopiedAddress(key);
        setTimeout(() => setCopiedAddress(null), 2000);
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
            {/* Backdrop */}
            <div
                className="absolute inset-0 bg-black/60 backdrop-blur-sm"
                onClick={onClose}
            />

            {/* Dialog - Theme aware */}
            <div className="relative bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-xl shadow-2xl w-full max-w-[540px] max-h-[90vh] flex flex-col overflow-hidden">
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 shrink-0">
                    <div className="flex items-center gap-2">
                        <Heart size={18} className="text-pink-500" />
                        <h2 className="text-base font-semibold">{t('support.title') || 'Support AeroFTP'}</h2>
                    </div>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700" title={t('common.close')}>
                        <X size={16} />
                    </button>
                </div>

                {/* Content */}
                <div className="overflow-y-auto flex-1 p-5 space-y-5">
                    {/* Fiat Section - Clean transparent buttons */}
                    <div>
                        <div className="flex items-center gap-2 mb-3">
                            <CreditCard size={16} className="text-gray-500 dark:text-gray-400" />
                            <h2 className="text-sm font-semibold text-gray-700 dark:text-gray-300">
                                {t('support.fiatSection') || 'Donate with Card'}
                            </h2>
                        </div>
                        <div className="grid grid-cols-2 gap-3">
                            {Object.entries(PAYMENT_LINKS).map(([key, link]) => (
                                <button
                                    key={key}
                                    onClick={() => openUrl(link.url)}
                                    className="flex flex-col items-center gap-2 p-4 rounded-xl bg-gray-100 dark:bg-gray-800/50 border border-gray-200 dark:border-gray-700 hover:bg-gray-200 dark:hover:bg-gray-800 hover:border-gray-300 dark:hover:border-gray-600 transition-all hover:scale-105"
                                >
                                    <link.Icon />
                                    <span className={`text-xs font-medium text-center leading-tight ${link.textColor}`}>
                                        {link.name}
                                    </span>
                                    <ExternalLink size={10} className="text-gray-400 dark:text-gray-500" />
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* Crypto Section */}
                    <div className="border-t border-gray-200 dark:border-gray-800 pt-4">
                        <div className="flex items-center gap-2 mb-3">
                            <CryptoWalletIcon />
                            <h2 className="text-sm font-semibold text-gray-700 dark:text-gray-300">
                                {t('support.cryptoSection') || 'Donate with Crypto'}
                            </h2>
                        </div>

                        {/* Crypto buttons - icons with proper colors */}
                        <div className="flex flex-wrap gap-2 mb-3">
                            {Object.entries(CRYPTO_ADDRESSES).map(([key, crypto]) => {
                                const IconComponent = crypto.Icon;
                                return (
                                    <button
                                        key={key}
                                        onClick={() => setSelectedCrypto(selectedCrypto === key ? null : key)}
                                        className={`flex items-center gap-2 px-3 py-2 rounded-lg border transition-all ${
                                            selectedCrypto === key
                                                ? 'bg-blue-100 dark:bg-gray-700 border-blue-500 text-blue-700 dark:text-white'
                                                : 'border-gray-200 dark:border-gray-700 bg-gray-100 dark:bg-gray-800/60 hover:bg-gray-200 dark:hover:bg-gray-800 text-gray-700 dark:text-gray-300'
                                        }`}
                                    >
                                        <IconComponent />
                                        <span className="text-sm font-medium">{crypto.symbol}</span>
                                    </button>
                                );
                            })}
                        </div>

                        {/* Selected crypto details with QR */}
                        {selectedCrypto && CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES] && (() => {
                            const crypto = CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES];
                            const IconComponent = crypto.Icon;
                            return (
                                <div className="bg-gray-100 dark:bg-gray-800/60 border border-gray-200 dark:border-gray-700 rounded-xl p-4 animate-slide-down">
                                    <div className="flex gap-4">
                                        {/* QR Code */}
                                        <div className="flex-shrink-0 bg-white p-2 rounded-lg shadow-sm">
                                            <QRCodeSVG
                                                value={crypto.address}
                                                size={100}
                                                level="M"
                                                includeMargin={false}
                                            />
                                        </div>

                                        {/* Address and copy */}
                                        <div className="flex-1 min-w-0">
                                            <div className="flex items-center gap-2 mb-2">
                                                <IconComponent />
                                                <span className="text-sm font-medium text-gray-700 dark:text-gray-200">
                                                    {crypto.name}
                                                </span>
                                            </div>
                                            <div className="bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg p-2 mb-2">
                                                <code className="text-xs text-green-600 dark:text-green-400 font-mono break-all select-all block">
                                                    {crypto.address}
                                                </code>
                                            </div>
                                            <button
                                                onClick={() => copyToClipboard(selectedCrypto, crypto.address)}
                                                className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-all ${
                                                    copiedAddress === selectedCrypto
                                                        ? 'bg-green-100 dark:bg-green-500/20 text-green-600 dark:text-green-400'
                                                        : 'bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-300'
                                                }`}
                                            >
                                                {copiedAddress === selectedCrypto ? (
                                                    <>
                                                        <Check size={14} />
                                                        {t('common.copied') || 'Copied!'}
                                                    </>
                                                ) : (
                                                    <>
                                                        <Copy size={14} />
                                                        {t('common.copy') || 'Copy address'}
                                                    </>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                </div>
                            );
                        })()}
                    </div>

                </div>

                {/* Footer */}
                <div className="px-5 py-2 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500 flex items-center justify-center gap-1 shrink-0">
                    <Heart size={12} className="text-pink-500" />
                    {t('support.thanks') || 'Thank you for your support!'}
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

export default SupportDialog;
