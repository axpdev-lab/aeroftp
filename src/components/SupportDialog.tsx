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

// Payment links
const PAYMENT_LINKS = {
    paypal: {
        name: 'PayPal',
        url: 'https://paypal.me/ale',
        icon: '💳',
        color: 'from-blue-500 to-blue-600',
        hoverColor: 'hover:from-blue-600 hover:to-blue-700',
    },
    github: {
        name: 'GitHub Sponsors',
        url: 'https://github.com/sponsors/axpnet',
        icon: '💜',
        color: 'from-purple-500 to-pink-500',
        hoverColor: 'hover:from-purple-600 hover:to-pink-600',
    },
    buymeacoffee: {
        name: 'Buy Me a Coffee',
        url: 'https://buymeacoffee.com/axpnet',
        icon: '☕',
        color: 'from-yellow-500 to-orange-500',
        hoverColor: 'hover:from-yellow-600 hover:to-orange-600',
    },
};

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

            {/* Dialog */}
            <div className="relative bg-gray-900 border border-gray-700 rounded-2xl shadow-2xl w-full max-w-lg overflow-hidden animate-scale-in max-h-[90vh] overflow-y-auto">
                {/* Header */}
                <div className="bg-gradient-to-br from-pink-500 via-rose-500 to-red-500 p-5 text-white text-center relative overflow-hidden">
                    {/* Grid overlay */}
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

                    {/* Heart icon */}
                    <div className="w-16 h-16 mx-auto mb-3 bg-white/20 backdrop-blur-sm rounded-2xl shadow-lg flex items-center justify-center border border-white/30">
                        <Heart size={32} className="text-white fill-white" />
                    </div>

                    <h1 className="text-xl font-bold">{t('support.title') || 'Supporta AeroFTP'}</h1>
                    <p className="text-pink-100 text-sm mt-1">
                        {t('support.subtitle') || 'Il tuo supporto aiuta a mantenere AeroFTP gratuito!'}
                    </p>
                </div>

                {/* Content */}
                <div className="p-5 space-y-5">
                    {/* Fiat Section */}
                    <div>
                        <div className="flex items-center gap-2 mb-3">
                            <CreditCard size={16} className="text-gray-400" />
                            <h2 className="text-sm font-semibold text-gray-300">
                                {t('support.fiatSection') || 'Dona con Carta'}
                            </h2>
                        </div>
                        <div className="grid grid-cols-3 gap-2">
                            {Object.entries(PAYMENT_LINKS).map(([key, link]) => (
                                <button
                                    key={key}
                                    onClick={() => openUrl(link.url)}
                                    className={`flex flex-col items-center gap-1.5 p-3 rounded-xl bg-gradient-to-br ${link.color} ${link.hoverColor} text-white transition-all shadow-md hover:shadow-lg hover:scale-105`}
                                >
                                    <span className="text-xl">{link.icon}</span>
                                    <span className="text-xs font-medium text-center leading-tight">{link.name}</span>
                                    <ExternalLink size={10} className="opacity-60" />
                                </button>
                            ))}
                        </div>
                    </div>

                    {/* Crypto Section */}
                    <div className="border-t border-gray-800 pt-4">
                        <div className="flex items-center gap-2 mb-3">
                            <Coffee size={16} className="text-gray-400" />
                            <h2 className="text-sm font-semibold text-gray-300">
                                {t('support.cryptoSection') || 'Dona con Crypto'}
                            </h2>
                        </div>

                        {/* Crypto buttons */}
                        <div className="flex flex-wrap gap-2 mb-3">
                            {Object.entries(CRYPTO_ADDRESSES).map(([key, crypto]) => (
                                <button
                                    key={key}
                                    onClick={() => setSelectedCrypto(selectedCrypto === key ? null : key)}
                                    className={`flex items-center gap-2 px-3 py-2 rounded-lg border transition-all ${
                                        selectedCrypto === key
                                            ? `bg-gradient-to-r ${crypto.color} border-transparent text-white`
                                            : 'border-gray-700 bg-gray-800/60 hover:bg-gray-800 text-gray-300'
                                    }`}
                                >
                                    <span className={`w-6 h-6 rounded-md bg-gradient-to-br ${crypto.color} flex items-center justify-center text-white text-xs font-bold`}>
                                        {crypto.icon}
                                    </span>
                                    <span className="text-sm font-medium">{crypto.symbol}</span>
                                </button>
                            ))}
                        </div>

                        {/* Selected crypto details with QR */}
                        {selectedCrypto && CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES] && (
                            <div className="bg-gray-800/60 border border-gray-700 rounded-xl p-4 animate-slide-down">
                                <div className="flex gap-4">
                                    {/* QR Code */}
                                    <div className="flex-shrink-0 bg-white p-2 rounded-lg">
                                        <QRCodeSVG
                                            value={CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].address}
                                            size={100}
                                            level="M"
                                            includeMargin={false}
                                        />
                                    </div>

                                    {/* Address and copy */}
                                    <div className="flex-1 min-w-0">
                                        <div className="flex items-center gap-2 mb-2">
                                            <span className={`w-6 h-6 rounded-md bg-gradient-to-br ${CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].color} flex items-center justify-center text-white text-xs font-bold`}>
                                                {CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].icon}
                                            </span>
                                            <span className="text-sm font-medium text-gray-200">
                                                {CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].name}
                                            </span>
                                        </div>
                                        <div className="bg-gray-900 border border-gray-700 rounded-lg p-2 mb-2">
                                            <code className="text-xs text-green-400 font-mono break-all select-all block">
                                                {CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].address}
                                            </code>
                                        </div>
                                        <button
                                            onClick={() => copyToClipboard(
                                                selectedCrypto,
                                                CRYPTO_ADDRESSES[selectedCrypto as keyof typeof CRYPTO_ADDRESSES].address
                                            )}
                                            className={`flex items-center gap-2 px-3 py-1.5 rounded-lg text-sm font-medium transition-all ${
                                                copiedAddress === selectedCrypto
                                                    ? 'bg-green-500/20 text-green-400'
                                                    : 'bg-gray-700 hover:bg-gray-600 text-gray-300'
                                            }`}
                                        >
                                            {copiedAddress === selectedCrypto ? (
                                                <>
                                                    <Check size={14} />
                                                    {t('common.copied') || 'Copiato!'}
                                                </>
                                            ) : (
                                                <>
                                                    <Copy size={14} />
                                                    {t('common.copy') || 'Copia indirizzo'}
                                                </>
                                            )}
                                        </button>
                                    </div>
                                </div>
                            </div>
                        )}
                    </div>

                    {/* Footer */}
                    <div className="text-center pt-3 border-t border-gray-800">
                        <p className="text-xs text-gray-500 flex items-center justify-center gap-1">
                            <Heart size={12} className="text-red-500" />
                            {t('support.thanks') || 'Grazie per il tuo supporto!'}
                        </p>
                        <p className="text-[10px] text-gray-600 mt-1 font-mono">
                            // {t('support.footer') || 'Ogni donazione aiuta lo sviluppo'}
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

export default SupportDialog;
