import { useState, useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import {
    X, Hash, Lock, KeyRound, Copy, Check, FileSearch, Type,
    RefreshCw, Eye, EyeOff, Loader2, AlertTriangle, CheckCircle2, Shuffle
} from 'lucide-react';
import { useTranslation } from '../i18n';

interface CyberToolsModalProps {
    onClose: () => void;
}

type TabId = 'hash' | 'crypto' | 'password';

export const CyberToolsModal: React.FC<CyberToolsModalProps> = ({ onClose }) => {
    const t = useTranslation();
    const [activeTab, setActiveTab] = useState<TabId>('hash');

    // Close on Escape
    useEffect(() => {
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [onClose]);

    const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
        { id: 'hash', label: t('cyberTools.hashForge'), icon: <Hash size={15} /> },
        { id: 'crypto', label: t('cyberTools.cryptoLab'), icon: <Lock size={15} /> },
        { id: 'password', label: t('cyberTools.passwordForge'), icon: <KeyRound size={15} /> },
    ];

    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60" onClick={onClose}>
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[560px] max-h-[85vh] flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <svg viewBox="0 0 120 120" width={18} height={18} fill="currentColor" className="text-cyan-500 dark:text-cyan-400">
                            <path d="M126.3,13.2C97.8,18 78.1,45.1 82.4,73.6c1.1,7.3 4.4,16.1 8.1,21.5l1,1.4-39.9,39.9c-21.9,22-40.3,40.7-40.7,41.5-0.5,1-0.8,2.6-0.8,4.4 0,7.9 8.3,12.5 15,8.1l2-1.3 9,8.9c8,7.9 9.2,8.9 11.2,9.4 1.2,0.3 2.5,0.6 3,0.6 3.2,0 7.2-2.7 8.7-5.8 0.9-2 1-6 0.1-8.1-0.4-0.9-2.4-3.4-4.5-5.7l-3.9-4 5.6-5.6 5.6-5.6 3.8,3.8c5.5,5.4 7.9,6.4 12.5,5 6.2-1.8 8.9-9.2 5.4-14.8-0.5-0.7-4.5-5-9-9.5l-8.1-8.1 18.6-18.7c17.6-17.6 18.7-18.9 19.8-21.6 1.8-4.4 3.9-7.8 7.3-11.3l3-3.2-4.2-4.2c-4.8-4.8-7.2-8.5-8.9-13.9-3-9.6-1.9-20 3.1-28.5 2.8-4.8 8.4-10.3 13.1-12.6 6.3-3.2 9.7-4.1 16.6-4.1 5,0 6.6,0.2 9.5,1.1 4.7,1.5 9.9,4.2 12.8,6.8l2.4,2.1 2.7-0.7c4.2-1.1 11.4-1.7 15.5-1.4 3.1,0.3 3.6,0.2 3.3-0.3-3-5.1-9-11.9-13.6-15.4-6.4-4.9-16.2-9.2-24-10.4-3.4-0.7-13.9-0.7-17.2-0.1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
                            <path d="M167.1,54.2c-15.1,3.4-25.7,12.2-29.9,24.7-1.2,3.6-1.2,3.9-1.4,19.5l-0.2,15.8h-4.9c-7.6,0-12.4,1.6-16.9,5.7-2.9,2.6-5.3,6.6-6.3,10.4-0.7,2.7-0.8,5.8-0.8,24.7 0,22.9 0.2,27.7 2.1,35.4 5.4,23 23,42.1 45.6,49.7 8.3,2.8 11.4,3.2 22,3.2 10.5,0 13.7-0.5 22-3.2 11-3.6 20.3-9.6 28.6-18.4 9-9.3 14.9-20.9 17.8-34.4 0.9-4.1 1-6.4 1.2-28.5 0.1-16.2 0-24.9-0.3-26.8-0.8-4.2-2.9-8.2-6-11.2-4.7-4.7-9.6-6.5-17.7-6.5h-5l-0.1-16.1c-0.1-16-0.1-16-1.4-19.8C211.8,67.2 201.2,58.1 187.9,54.6 182.3,53.2 172.6,53 167.1,54.2zM184.8,74.1c6,1.8 11.1,6.4 12.4,11 0.3,1 0.5,7.5 0.5,15.4v13.8h-21-21V99.7c0-16.6 0-16.3 3.7-20.3 2.7-2.8 6.8-5.1 11.2-6.1 3.7-0.8 10.2-0.5 14.2,0.8zM181.7,152.9c2.4,1.2 5.4,4.6 6.2,6.8 0.8,2.5 0.7,6.1-0.2,8.6-0.8,2-4.8,6.4-5.9,6.4-0.8,0-0.5,1.2 2.8,9 1.8,4.3 3.2,8.1 3.2,8.6 0,3.1-1.6,3.6-11.3,3.6h-8.2l-1.3-1.3c-0.7-0.7-1.3-1.7-1.3-2.1 0-0.4 1.4-4.4 3.1-9l3.2-8.3-1.6-0.9c-3.6-1.9-6.1-6.2-6.1-10.6 0-5.5 3.4-10.1 8.8-11.8 1.9-0.5 6.6,0 8.6,1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
                        </svg>
                        <span className="font-medium text-gray-900 dark:text-gray-100">{t('cyberTools.title')}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors cursor-pointer">
                        <X size={18} className="text-gray-500" />
                    </button>
                </div>

                {/* Tabs */}
                <div className="flex border-b border-gray-200 dark:border-gray-700 px-2">
                    {tabs.map(tab => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id)}
                            className={`flex items-center gap-1.5 px-3 py-2 text-sm font-medium transition-colors border-b-2 cursor-pointer ${
                                activeTab === tab.id
                                    ? 'border-cyan-500 text-cyan-600 dark:text-cyan-400'
                                    : 'border-transparent text-gray-500 hover:text-gray-700 dark:hover:text-gray-300'
                            }`}
                        >
                            {tab.icon}
                            {tab.label}
                        </button>
                    ))}
                </div>

                {/* Content */}
                <div className="p-4 overflow-y-auto flex-1">
                    {activeTab === 'hash' && <HashForgeTab />}
                    {activeTab === 'crypto' && <CryptoLabTab />}
                    {activeTab === 'password' && <PasswordForgeTab />}
                </div>
            </div>
        </div>
    );
};

// ─── Shared Components ──────────────────────────────────────────────────────

const CopyButton: React.FC<{ text: string; label?: string }> = ({ text, label }) => {
    const [copied, setCopied] = useState(false);
    const handleCopy = useCallback(async () => {
        try {
            await invoke('copy_to_clipboard', { text });
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        } catch { /* clipboard may fail in some environments */ }
    }, [text]);

    return (
        <button
            onClick={handleCopy}
            className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
            title={label}
        >
            {copied ? <Check size={12} className="text-green-500" /> : <Copy size={12} />}
            {copied ? 'Copied!' : (label || 'Copy')}
        </button>
    );
};

const PillButton: React.FC<{ active: boolean; onClick: () => void; children: React.ReactNode }> = ({ active, onClick, children }) => (
    <button
        onClick={onClick}
        className={`px-3 py-1 text-xs font-medium rounded-full transition-colors cursor-pointer ${
            active
                ? 'bg-cyan-500 text-white'
                : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'
        }`}
    >
        {children}
    </button>
);

// ─── Hash Forge Tab ─────────────────────────────────────────────────────────

const HASH_ALGOS = ['MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3'] as const;

const HashForgeTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'text' | 'file'>('text');
    const [input, setInput] = useState('');
    const [filePath, setFilePath] = useState('');
    const [algorithm, setAlgorithm] = useState('sha256');
    const [result, setResult] = useState('');
    const [expected, setExpected] = useState('');
    const [match, setMatch] = useState<boolean | null>(null);
    const [loading, setLoading] = useState(false);

    const algoMap: Record<string, string> = { 'MD5': 'md5', 'SHA-1': 'sha1', 'SHA-256': 'sha256', 'SHA-512': 'sha512', 'BLAKE3': 'blake3' };

    const calculate = useCallback(async () => {
        setLoading(true);
        setResult('');
        setMatch(null);
        try {
            let hash: string;
            if (mode === 'text') {
                hash = await invoke('hash_text', { text: input, algorithm });
            } else {
                hash = await invoke('hash_file', { path: filePath, algorithm });
            }
            setResult(hash);
            if (expected.trim()) {
                const isMatch: boolean = await invoke('compare_hashes', { hashA: hash, hashB: expected.trim() });
                setMatch(isMatch);
            }
        } catch (e) {
            setResult(`Error: ${e}`);
        }
        setLoading(false);
    }, [mode, input, filePath, algorithm, expected]);

    const selectFile = useCallback(async () => {
        const selected = await open({ multiple: false, directory: false });
        if (selected) setFilePath(selected as string);
    }, []);

    // Auto-compare when expected changes
    useEffect(() => {
        if (result && expected.trim()) {
            invoke<boolean>('compare_hashes', { hashA: result, hashB: expected.trim() }).then(setMatch);
        } else {
            setMatch(null);
        }
    }, [expected, result]);

    return (
        <div className="space-y-4">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.hashDescription')}</p>

            {/* Mode toggle */}
            <div className="flex gap-2">
                <PillButton active={mode === 'text'} onClick={() => setMode('text')}>
                    <span className="flex items-center gap-1"><Type size={12} /> {t('cyberTools.hashModeText')}</span>
                </PillButton>
                <PillButton active={mode === 'file'} onClick={() => setMode('file')}>
                    <span className="flex items-center gap-1"><FileSearch size={12} /> {t('cyberTools.hashModeFile')}</span>
                </PillButton>
            </div>

            {/* Input */}
            {mode === 'text' ? (
                <textarea
                    value={input}
                    onChange={e => setInput(e.target.value)}
                    placeholder={t('cyberTools.hashInputPlaceholder')}
                    className="w-full h-24 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 resize-none focus:outline-none focus:ring-1 focus:ring-cyan-500"
                />
            ) : (
                <div className="flex gap-2">
                    <input
                        value={filePath}
                        readOnly
                        placeholder={t('cyberTools.hashSelectFile')}
                        className="flex-1 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 truncate"
                    />
                    <button
                        onClick={selectFile}
                        className="px-3 py-2 text-sm rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                    >
                        <FileSearch size={16} />
                    </button>
                </div>
            )}

            {/* Algorithm */}
            <div>
                <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashAlgorithm')}</label>
                <div className="flex flex-wrap gap-1.5">
                    {HASH_ALGOS.map(a => (
                        <PillButton key={a} active={algorithm === algoMap[a]} onClick={() => setAlgorithm(algoMap[a])}>
                            {a}
                        </PillButton>
                    ))}
                </div>
            </div>

            {/* Calculate */}
            <button
                onClick={calculate}
                disabled={loading || (mode === 'text' ? !input : !filePath)}
                className="w-full py-2 text-sm font-medium rounded bg-cyan-500 hover:bg-cyan-600 disabled:bg-gray-300 dark:disabled:bg-gray-700 text-white transition-colors cursor-pointer disabled:cursor-not-allowed flex items-center justify-center gap-2"
            >
                {loading ? <><Loader2 size={14} className="animate-spin" /> {t('cyberTools.hashCalculating')}</> : t('cyberTools.hashCalculate')}
            </button>

            {/* Result */}
            {result && (
                <div className="space-y-2">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.hashResult')}</label>
                    <div className="flex items-start gap-2">
                        <code className="flex-1 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 text-gray-800 dark:text-gray-200 break-all border border-gray-200 dark:border-gray-700 select-all">
                            {result}
                        </code>
                        <CopyButton text={result} label={t('cyberTools.hashCopy')} />
                    </div>
                </div>
            )}

            {/* Compare */}
            {result && (
                <div className="space-y-1">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.hashExpected')}</label>
                    <input
                        value={expected}
                        onChange={e => setExpected(e.target.value)}
                        placeholder={t('cyberTools.hashExpectedPlaceholder')}
                        className={`w-full px-3 py-2 text-xs font-mono rounded border bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 ${
                            match === true ? 'border-green-500 focus:ring-green-500' :
                            match === false ? 'border-red-500 focus:ring-red-500' :
                            'border-gray-300 dark:border-gray-600 focus:ring-cyan-500'
                        }`}
                    />
                    {match === true && (
                        <div className="flex items-center gap-1 text-xs text-green-500">
                            <CheckCircle2 size={12} /> {t('cyberTools.hashMatch')}
                        </div>
                    )}
                    {match === false && (
                        <div className="flex items-center gap-1 text-xs text-red-500">
                            <AlertTriangle size={12} /> {t('cyberTools.hashMismatch')}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

// ─── CryptoLab Tab ──────────────────────────────────────────────────────────

const CryptoLabTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'encrypt' | 'decrypt'>('encrypt');
    const [algorithm, setAlgorithm] = useState('aes-256-gcm');
    const [input, setInput] = useState('');
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [output, setOutput] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState('');

    const execute = useCallback(async () => {
        setError('');
        setOutput('');
        if (!input.trim()) { setError(t('cyberTools.cryptoNoInput')); return; }
        if (!password) { setError(t('cyberTools.cryptoNoPassword')); return; }

        setLoading(true);
        try {
            if (mode === 'encrypt') {
                const result: string = await invoke('crypto_encrypt_text', {
                    plaintext: input, password, algorithm
                });
                setOutput(result);
            } else {
                const result: string = await invoke('crypto_decrypt_text', {
                    encoded: input.trim(), password
                });
                setOutput(result);
            }
        } catch (e) {
            setError(String(e));
        }
        setLoading(false);
    }, [mode, algorithm, input, password, t]);

    return (
        <div className="space-y-4">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.cryptoDescription')}</p>

            {/* Mode */}
            <div className="flex gap-2">
                <PillButton active={mode === 'encrypt'} onClick={() => { setMode('encrypt'); setInput(''); setOutput(''); setError(''); }}>
                    {t('cyberTools.cryptoEncrypt')}
                </PillButton>
                <PillButton active={mode === 'decrypt'} onClick={() => { setMode('decrypt'); setInput(''); setOutput(''); setError(''); }}>
                    {t('cyberTools.cryptoDecrypt')}
                </PillButton>
            </div>

            {/* Algorithm (only for encrypt) */}
            {mode === 'encrypt' && (
                <div>
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.cryptoAlgorithm')}</label>
                    <div className="flex gap-1.5">
                        <PillButton active={algorithm === 'aes-256-gcm'} onClick={() => setAlgorithm('aes-256-gcm')}>AES-256-GCM</PillButton>
                        <PillButton active={algorithm === 'chacha20-poly1305'} onClick={() => setAlgorithm('chacha20-poly1305')}>ChaCha20-Poly1305</PillButton>
                    </div>
                </div>
            )}

            {/* Input */}
            <textarea
                value={input}
                onChange={e => setInput(e.target.value)}
                placeholder={mode === 'encrypt' ? t('cyberTools.cryptoInputPlaceholder') : t('cyberTools.cryptoCiphertextPlaceholder')}
                className="w-full h-24 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 resize-none focus:outline-none focus:ring-1 focus:ring-cyan-500 font-mono"
            />

            {/* Password */}
            <div>
                <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.cryptoPassword')}</label>
                <div className="relative">
                    <input
                        type={showPassword ? 'text' : 'password'}
                        value={password}
                        onChange={e => setPassword(e.target.value)}
                        placeholder={t('cyberTools.cryptoPasswordPlaceholder')}
                        className="w-full px-3 py-2 pr-10 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                    />
                    <button
                        onClick={() => setShowPassword(!showPassword)}
                        className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 cursor-pointer"
                    >
                        {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                </div>
            </div>

            {/* KDF info */}
            <p className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.cryptoKdfInfo')}</p>

            {/* Execute */}
            <button
                onClick={execute}
                disabled={loading}
                className="w-full py-2 text-sm font-medium rounded bg-cyan-500 hover:bg-cyan-600 disabled:bg-gray-300 dark:disabled:bg-gray-700 text-white transition-colors cursor-pointer disabled:cursor-not-allowed flex items-center justify-center gap-2"
            >
                {loading ? (
                    <><Loader2 size={14} className="animate-spin" /> {mode === 'encrypt' ? t('cyberTools.cryptoEncrypting') : t('cyberTools.cryptoDecrypting')}</>
                ) : (
                    mode === 'encrypt' ? t('cyberTools.cryptoEncrypt') : t('cyberTools.cryptoDecrypt')
                )}
            </button>

            {/* Error */}
            {error && (
                <div className="flex items-center gap-1.5 text-xs text-red-500">
                    <AlertTriangle size={12} /> {error}
                </div>
            )}

            {/* Output */}
            {output && (
                <div className="space-y-2">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.cryptoResult')}</label>
                    <div className="flex items-start gap-2">
                        <code className="flex-1 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 text-gray-800 dark:text-gray-200 break-all border border-gray-200 dark:border-gray-700 select-all max-h-40 overflow-y-auto">
                            {output}
                        </code>
                        <CopyButton text={output} label={t('cyberTools.cryptoCopy')} />
                    </div>
                </div>
            )}
        </div>
    );
};

// ─── Password Forge Tab ─────────────────────────────────────────────────────

const PasswordForgeTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'random' | 'passphrase'>('random');
    const [length, setLength] = useState(24);
    const [uppercase, setUppercase] = useState(true);
    const [lowercase, setLowercase] = useState(true);
    const [digits, setDigits] = useState(true);
    const [symbols, setSymbols] = useState(true);
    const [excludeAmbiguous, setExcludeAmbiguous] = useState(false);
    const [wordCount, setWordCount] = useState(5);
    const [separator, setSeparator] = useState('-');
    const [capitalize, setCapitalize] = useState(true);
    const [batchCount, setBatchCount] = useState(1);
    const [passwords, setPasswords] = useState<string[]>([]);
    const [entropy, setEntropy] = useState(0);
    const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
    const clearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Calculate entropy when settings change
    useEffect(() => {
        if (mode === 'random') {
            invoke<number>('calculate_entropy', {
                length, uppercase, lowercase, digits, symbols, excludeAmbiguous: excludeAmbiguous
            }).then(setEntropy).catch(() => setEntropy(0));
        } else {
            // Passphrase entropy: log2(wordlist_size) * word_count
            // Our wordlist has ~1000 words
            setEntropy(wordCount * Math.log2(1000));
        }
    }, [mode, length, uppercase, lowercase, digits, symbols, excludeAmbiguous, wordCount]);

    const generate = useCallback(async () => {
        try {
            if (mode === 'random') {
                const result: string[] = await invoke('generate_password', {
                    length, uppercase, lowercase, digits, symbols,
                    excludeAmbiguous: excludeAmbiguous, count: batchCount
                });
                setPasswords(result);
            } else {
                const result: string[] = await invoke('generate_passphrase', {
                    wordCount, separator, capitalize, count: batchCount
                });
                setPasswords(result);
            }
        } catch (e) {
            setPasswords([`Error: ${e}`]);
        }
    }, [mode, length, uppercase, lowercase, digits, symbols, excludeAmbiguous, wordCount, separator, capitalize, batchCount]);

    const copyPassword = useCallback(async (pwd: string, idx: number) => {
        try {
            await invoke('copy_to_clipboard', { text: pwd });
            setCopiedIdx(idx);
            setTimeout(() => setCopiedIdx(null), 2000);
            // Auto-clear clipboard after 30s
            if (clearTimerRef.current) clearTimeout(clearTimerRef.current);
            clearTimerRef.current = setTimeout(async () => {
                try { await invoke('copy_to_clipboard', { text: '' }); } catch { /* ignore */ }
            }, 30000);
        } catch { /* ignore */ }
    }, []);

    // Cleanup timer on unmount
    useEffect(() => {
        return () => { if (clearTimerRef.current) clearTimeout(clearTimerRef.current); };
    }, []);

    const entropyColor = entropy < 40 ? 'bg-red-500' : entropy < 60 ? 'bg-orange-500' : entropy < 80 ? 'bg-yellow-500' : entropy < 100 ? 'bg-green-500' : 'bg-cyan-500';
    const entropyLabel = entropy < 40 ? t('cyberTools.pwdWeak') : entropy < 60 ? t('cyberTools.pwdFair') : entropy < 80 ? t('cyberTools.pwdGood') : entropy < 100 ? t('cyberTools.pwdStrong') : t('cyberTools.pwdExcellent');
    const entropyPct = Math.min(100, (entropy / 128) * 100);

    return (
        <div className="space-y-4">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.pwdDescription')}</p>

            {/* Mode */}
            <div className="flex gap-2">
                <PillButton active={mode === 'random'} onClick={() => setMode('random')}>{t('cyberTools.pwdModeRandom')}</PillButton>
                <PillButton active={mode === 'passphrase'} onClick={() => setMode('passphrase')}>{t('cyberTools.pwdModePassphrase')}</PillButton>
            </div>

            {mode === 'random' ? (
                <div className="space-y-3">
                    {/* Length slider */}
                    <div>
                        <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-1">
                            <span>{t('cyberTools.pwdLength')}</span>
                            <span className="font-mono">{length}</span>
                        </div>
                        <input
                            type="range" min={8} max={128} value={length}
                            onChange={e => setLength(Number(e.target.value))}
                            className="w-full accent-cyan-500"
                        />
                    </div>

                    {/* Checkboxes */}
                    <div className="grid grid-cols-2 gap-2">
                        {[
                            { label: t('cyberTools.pwdUppercase'), checked: uppercase, set: setUppercase },
                            { label: t('cyberTools.pwdLowercase'), checked: lowercase, set: setLowercase },
                            { label: t('cyberTools.pwdDigits'), checked: digits, set: setDigits },
                            { label: t('cyberTools.pwdSymbols'), checked: symbols, set: setSymbols },
                        ].map(({ label, checked, set }) => (
                            <label key={label} className="flex items-center gap-2 text-xs text-gray-700 dark:text-gray-300 cursor-pointer">
                                <input type="checkbox" checked={checked} onChange={e => set(e.target.checked)} className="rounded accent-cyan-500" />
                                {label}
                            </label>
                        ))}
                        <label className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400 col-span-2 cursor-pointer">
                            <input type="checkbox" checked={excludeAmbiguous} onChange={e => setExcludeAmbiguous(e.target.checked)} className="rounded accent-cyan-500" />
                            {t('cyberTools.pwdExcludeAmbiguous')}
                        </label>
                    </div>
                </div>
            ) : (
                <div className="space-y-3">
                    {/* Word count */}
                    <div>
                        <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-1">
                            <span>{t('cyberTools.pwdWordCount')}</span>
                            <span className="font-mono">{wordCount}</span>
                        </div>
                        <input
                            type="range" min={3} max={24} value={wordCount}
                            onChange={e => setWordCount(Number(e.target.value))}
                            className="w-full accent-cyan-500"
                        />
                    </div>
                    <div className="flex gap-3">
                        <div className="flex-1">
                            <label className="text-xs text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.pwdSeparator')}</label>
                            <input
                                value={separator}
                                onChange={e => setSeparator(e.target.value)}
                                maxLength={3}
                                className="w-full px-3 py-1.5 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-center font-mono"
                            />
                        </div>
                        <label className="flex items-center gap-2 text-xs text-gray-700 dark:text-gray-300 cursor-pointer pt-5">
                            <input type="checkbox" checked={capitalize} onChange={e => setCapitalize(e.target.checked)} className="rounded accent-cyan-500" />
                            {t('cyberTools.pwdCapitalize')}
                        </label>
                    </div>
                    {wordCount >= 12 && (
                        <p className="text-[10px] text-amber-500/80 mt-1">{t('cyberTools.pwdNotBip39')}</p>
                    )}
                </div>
            )}

            {/* Batch count */}
            <div>
                <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-1">
                    <span>{t('cyberTools.pwdBatchCount')}</span>
                    <span className="font-mono">{batchCount}</span>
                </div>
                <input
                    type="range" min={1} max={5} value={batchCount}
                    onChange={e => setBatchCount(Number(e.target.value))}
                    className="w-full accent-cyan-500"
                />
            </div>

            {/* Entropy bar */}
            <div>
                <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-1">
                    <span>{t('cyberTools.pwdEntropy')}</span>
                    <span className="font-mono">{Math.round(entropy)} {t('cyberTools.pwdBits')} — {entropyLabel}</span>
                </div>
                <div className="h-2 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                    <div className={`h-full rounded-full transition-all duration-300 ${entropyColor}`} style={{ width: `${entropyPct}%` }} />
                </div>
            </div>

            {/* Generate */}
            <button
                onClick={generate}
                className="w-full py-2 text-sm font-medium rounded bg-cyan-500 hover:bg-cyan-600 text-white transition-colors cursor-pointer flex items-center justify-center gap-2"
            >
                <Shuffle size={14} /> {t('cyberTools.pwdGenerate')}
            </button>

            {/* Results */}
            {passwords.length > 0 && (
                <div className="space-y-2">
                    {passwords.map((pwd, i) => (
                        <div key={i} className="flex items-center gap-2">
                            <code className="flex-1 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 text-gray-800 dark:text-gray-200 break-all border border-gray-200 dark:border-gray-700 select-all truncate">
                                {pwd}
                            </code>
                            <button
                                onClick={() => copyPassword(pwd, i)}
                                className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer shrink-0"
                            >
                                {copiedIdx === i ? <Check size={12} className="text-green-500" /> : <Copy size={12} />}
                                {copiedIdx === i ? t('cyberTools.pwdCopied') : t('cyberTools.pwdCopy')}
                            </button>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
};

export default CyberToolsModal;
