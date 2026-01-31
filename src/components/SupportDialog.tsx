/**
 * SupportDialog Component
 * Donation dialog with fiat (PayPal, GitHub Sponsors, Buy Me a Coffee) and crypto options with QR codes
 */

import * as React from 'react';
import { useState } from 'react';
import { X, Heart, Copy, Check, ExternalLink, Coffee, CreditCard } from 'lucide-react';
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

// Official Buy Me a Coffee SVG Icon
const BuyMeACoffeeIcon = () => (
    <svg viewBox="0 0 600 450" className="w-8 h-8">
        <path fill="#FFDD06" d="M308,212.8c-11.8,5.1-25.3,10.8-42.7,10.8c-7.3,0-14.5-1-21.5-3l12,123.6c0.4,5.2,2.8,10,6.6,13.5c3.8,3.5,8.8,5.5,14,5.5c0,0,17.1,0.9,22.8,0.9c6.1,0,24.5-0.9,24.5-0.9c5.2,0,10.2-1.9,14-5.5c3.8-3.5,6.2-8.3,6.6-13.5l12.9-136.5c-5.8-2-11.6-3.3-18.1-3.3C327.7,204.4,318.6,208.3,308,212.8z"/>
        <path fill="#010202" d="M412.8,148.7l-1.8-9.1c-1.6-8.2-5.3-16-13.7-18.9c-2.7-0.9-5.8-1.4-7.8-3.3c-2.1-2-2.7-5-3.2-7.8c-0.9-5.2-1.7-10.4-2.6-15.6c-0.8-4.5-1.4-9.5-3.4-13.5c-2.7-5.5-8.2-8.7-13.7-10.8c-2.8-1-5.7-1.9-8.6-2.7c-13.7-3.6-28.1-4.9-42.2-5.7c-16.9-0.9-33.9-0.7-50.8,0.8c-12.6,1.1-25.8,2.5-37.7,6.9c-4.4,1.6-8.9,3.5-12.2,6.9c-4.1,4.1-5.4,10.6-2.4,15.7c2.1,3.7,5.7,6.3,9.5,8c4.9,2.2,10.1,3.9,15.4,5c14.8,3.3,30,4.5,45.1,5.1c16.7,0.7,33.4,0.1,50.1-1.6c4.1-0.5,8.2-1,12.3-1.6c4.8-0.7,7.9-7.1,6.5-11.4c-1.7-5.3-6.3-7.3-11.4-6.5c-0.8,0.1-1.5,0.2-2.3,0.3l-0.5,0.1c-1.8,0.2-3.5,0.4-5.3,0.6c-3.6,0.4-7.2,0.7-10.9,1c-8.1,0.6-16.3,0.8-24.5,0.8c-8,0-16-0.2-24-0.8c-3.7-0.2-7.3-0.5-10.9-0.9c-1.7-0.2-3.3-0.4-4.9-0.6l-1.6-0.2l-0.3,0l-1.6-0.2c-3.3-0.5-6.6-1.1-9.9-1.8c-0.3-0.1-0.6-0.3-0.8-0.5s-0.3-0.6-0.3-0.9c0-0.3,0.1-0.7,0.3-0.9s0.5-0.4,0.8-0.5h0.1c2.8-0.6,5.7-1.1,8.6-1.6c1-0.2,1.9-0.3,2.9-0.4h0c1.8-0.1,3.6-0.4,5.4-0.7c15.6-1.6,31.3-2.2,47-1.7c7.6,0.2,15.2,0.7,22.8,1.4c1.6,0.2,3.3,0.3,4.9,0.5c0.6,0.1,1.2,0.2,1.9,0.2l1.3,0.2c3.7,0.5,7.3,1.2,11,2c5.4,1.2,12.3,1.6,14.7,7.4c0.8,1.9,1.1,3.9,1.5,5.9l0.5,2.5c0,0,0,0.1,0,0.1c1.3,5.9,2.5,11.8,3.8,17.7c0.1,0.4,0.1,0.9,0,1.3c-0.1,0.4-0.3,0.9-0.5,1.2c-0.3,0.4-0.6,0.7-1,0.9c-0.4,0.2-0.8,0.4-1.2,0.4h0l-0.8,0.1l-0.8,0.1c-2.4,0.3-4.9,0.6-7.3,0.9c-4.8,0.5-9.6,1-14.4,1.4c-9.6,0.8-19.1,1.3-28.7,1.6c-4.9,0.1-9.8,0.2-14.7,0.2c-19.5,0-38.9-1.1-58.2-3.4c-2.1-0.2-4.2-0.5-6.3-0.8c1.6,0.2-1.2-0.2-1.7-0.2c-1.3-0.2-2.7-0.4-4-0.6c-4.5-0.7-8.9-1.5-13.4-2.2c-5.4-0.9-10.5-0.4-15.4,2.2c-4,2.2-7.2,5.5-9.3,9.6c-2.1,4.3-2.7,9.1-3.7,13.7c-0.9,4.7-2.4,9.7-1.8,14.5c1.2,10.3,8.4,18.7,18.8,20.6c9.8,1.8,19.6,3.2,29.5,4.4c38.7,4.7,77.9,5.3,116.7,1.7c3.2-0.3,6.3-0.6,9.5-1c1-0.1,2,0,2.9,0.3c0.9,0.3,1.8,0.9,2.5,1.6c0.7,0.7,1.2,1.5,1.6,2.5c0.3,0.9,0.5,1.9,0.4,2.9l-1,9.6c-2,19.3-4,38.6-5.9,58c-2.1,20.3-4.1,40.6-6.2,60.9c-0.6,5.7-1.2,11.4-1.8,17.1c-0.6,5.6-0.6,11.4-1.7,17c-1.7,8.7-7.6,14.1-16.2,16.1c-7.9,1.8-16,2.7-24.1,2.8c-9,0-18-0.3-27-0.3c-9.6,0.1-21.4-0.8-28.8-8c-6.5-6.3-7.4-16.1-8.3-24.6c-1.2-11.2-2.4-22.5-3.5-33.7l-6.5-62.5l-4.2-40.5c-0.1-0.7-0.1-1.3-0.2-2c-0.5-4.8-3.9-9.5-9.3-9.3c-4.6,0.2-9.8,4.1-9.3,9.3l3.1,30l6.5,62c1.8,17.6,3.7,35.2,5.5,52.9c0.4,3.4,0.7,6.8,1.1,10.1c2,18.5,16.1,28.4,33.6,31.2c10.2,1.6,20.6,2,31,2.1c13.3,0.2,26.7,0.7,39.7-1.7c19.3-3.5,33.8-16.4,35.9-36.5c0.6-5.8,1.2-11.6,1.8-17.3c2-19.1,3.9-38.2,5.9-57.4l6.4-62.5l2.9-28.6c0.1-1.4,0.7-2.8,1.7-3.8s2.2-1.8,3.6-2c5.5-1.1,10.8-2.9,14.7-7.1C413.8,166.2,415,157.5,412.8,148.7z M392.5,159.3c-2,1.9-5,2.8-7.9,3.2c-33.1,4.9-66.8,7.4-100.3,6.3c-24-0.8-47.7-3.5-71.5-6.8c-2.3-0.3-4.8-0.8-6.4-2.5c-3-3.2-1.5-9.7-0.7-13.7c0.7-3.6,2.1-8.4,6.4-8.9c6.6-0.8,14.4,2,20.9,3c7.9,1.2,15.9,2.2,23.8,2.9c34,3.1,68.7,2.6,102.5-1.9c6.2-0.8,12.3-1.8,18.5-2.9c5.5-1,11.5-2.8,14.8,2.8c2.3,3.9,2.6,9,2.2,13.4C394.8,156.2,393.9,158,392.5,159.3L392.5,159.3z"/>
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
        Icon: BuyMeACoffeeIcon,
        textColor: 'text-[#FFDD06]',
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
        <div className="fixed inset-0 z-50 flex items-center justify-center">
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
                            <Coffee size={16} className="text-gray-500 dark:text-gray-400" />
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
