import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { Shield, Lock, Eye, EyeOff, ShieldCheck, AlertCircle } from 'lucide-react';
import { useTranslation } from '../i18n';

// ============ Lock Screen Background Patterns ============
// Each pattern is a lightweight inline SVG data URI.
// To add a new pattern: add an entry here with id, nameKey (i18n), and svg.
// To remove: delete the entry. The component reads the selected id from localStorage.

export interface LockPattern {
    id: string;
    nameKey: string;
    svg: string;
}

export const LOCK_SCREEN_PATTERNS: LockPattern[] = [
    {
        id: 'cross',
        nameKey: 'lockScreen.patternCross',
        svg: `url("data:image/svg+xml,%3Csvg width='60' height='60' viewBox='0 0 60 60' xmlns='http://www.w3.org/2000/svg'%3E%3Cg fill='none' fill-rule='evenodd'%3E%3Cg fill='%23ffffff' fill-opacity='1'%3E%3Cpath d='M36 34v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zm0-30V0h-2v4h-4v2h4v4h2V6h4V4h-4zM6 34v-4H4v4H0v2h4v4h2v-4h4v-2H6zM6 4V0H4v4H0v2h4v4h2V6h4V4H6z'/%3E%3C/g%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'dots',
        nameKey: 'lockScreen.patternDots',
        svg: `url("data:image/svg+xml,%3Csvg width='20' height='20' viewBox='0 0 20 20' xmlns='http://www.w3.org/2000/svg'%3E%3Ccircle cx='10' cy='10' r='1.5' fill='%23ffffff'/%3E%3C/svg%3E")`,
    },
    {
        id: 'circuit',
        nameKey: 'lockScreen.patternCircuit',
        svg: `url("data:image/svg+xml,%3Csvg width='80' height='80' viewBox='0 0 80 80' xmlns='http://www.w3.org/2000/svg'%3E%3Cg fill='none' stroke='%23ffffff' stroke-width='0.8'%3E%3Crect x='10' y='10' width='20' height='20' rx='2'/%3E%3Crect x='50' y='50' width='20' height='20' rx='2'/%3E%3Cline x1='30' y1='20' x2='50' y2='20'/%3E%3Cline x1='50' y1='20' x2='50' y2='50'/%3E%3Cline x1='20' y1='30' x2='20' y2='50'/%3E%3Cline x1='20' y1='50' x2='50' y2='50'/%3E%3Ccircle cx='30' cy='20' r='2' fill='%23ffffff'/%3E%3Ccircle cx='50' cy='50' r='2' fill='%23ffffff'/%3E%3Ccircle cx='20' cy='30' r='2' fill='%23ffffff'/%3E%3Ccircle cx='20' cy='50' r='2' fill='%23ffffff'/%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'diagonal',
        nameKey: 'lockScreen.patternDiagonal',
        svg: `url("data:image/svg+xml,%3Csvg width='40' height='40' viewBox='0 0 40 40' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M0 40L40 0M-10 10L10-10M30 50L50 30' stroke='%23ffffff' stroke-width='0.8' fill='none'/%3E%3C/svg%3E")`,
    },
    {
        id: 'hexagon',
        nameKey: 'lockScreen.patternHexagon',
        svg: `url("data:image/svg+xml,%3Csvg width='28' height='49' viewBox='0 0 28 49' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M14 0L28 8.5V25.5L14 34L0 25.5V8.5L14 0zM14 15L28 23.5V40.5L14 49L0 40.5V23.5L14 15z' stroke='%23ffffff' stroke-width='0.6' fill='none'/%3E%3C/svg%3E")`,
    },
    {
        id: 'grid',
        nameKey: 'lockScreen.patternGrid',
        svg: `url("data:image/svg+xml,%3Csvg width='40' height='40' viewBox='0 0 40 40' xmlns='http://www.w3.org/2000/svg'%3E%3Crect width='40' height='40' fill='none' stroke='%23ffffff' stroke-width='0.5'/%3E%3C/svg%3E")`,
    },
    {
        id: 'topography',
        nameKey: 'lockScreen.patternTopography',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M10 50c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.6' fill='none'/%3E%3Cpath d='M10 30c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.6' fill='none'/%3E%3Cpath d='M10 70c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.6' fill='none'/%3E%3C/svg%3E")`,
    },
    {
        id: 'waves',
        nameKey: 'lockScreen.patternWaves',
        svg: `url("data:image/svg+xml,%3Csvg width='120' height='60' viewBox='0 0 120 60' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M0 30 Q30 10 60 30 T120 30' stroke='%23ffffff' stroke-width='0.6' fill='none'/%3E%3Cpath d='M0 45 Q30 25 60 45 T120 45' stroke='%23ffffff' stroke-width='0.6' fill='none' opacity='0.7'/%3E%3Cpath d='M0 15 Q30 -5 60 15 T120 15' stroke='%23ffffff' stroke-width='0.6' fill='none' opacity='0.7'/%3E%3C/svg%3E")`,
    },
    {
        id: 'constellation',
        nameKey: 'lockScreen.patternConstellation',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cg%3E%3Ccircle cx='20' cy='20' r='1.5' fill='%23ffffff'/%3E%3Ccircle cx='80' cy='30' r='1' fill='%23ffffff'/%3E%3Ccircle cx='50' cy='50' r='2' fill='%23ffffff'/%3E%3Ccircle cx='30' cy='80' r='1' fill='%23ffffff'/%3E%3Ccircle cx='90' cy='70' r='1.5' fill='%23ffffff'/%3E%3Ccircle cx='10' cy='60' r='1' fill='%23ffffff'/%3E%3Cline x1='20' y1='20' x2='50' y2='50' stroke='%23ffffff' stroke-width='0.4'/%3E%3Cline x1='80' y1='30' x2='50' y2='50' stroke='%23ffffff' stroke-width='0.4'/%3E%3Cline x1='50' y1='50' x2='30' y2='80' stroke='%23ffffff' stroke-width='0.4'/%3E%3Cline x1='50' y1='50' x2='90' y2='70' stroke='%23ffffff' stroke-width='0.4'/%3E%3Cline x1='20' y1='20' x2='10' y2='60' stroke='%23ffffff' stroke-width='0.4'/%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'isometric',
        nameKey: 'lockScreen.patternIsometric',
        svg: `url("data:image/svg+xml,%3Csvg width='60' height='52' viewBox='0 0 60 52' xmlns='http://www.w3.org/2000/svg'%3E%3Cg stroke='%23ffffff' stroke-width='0.5' fill='none'%3E%3Cpath d='M30 0 L60 17.3 L60 52 L30 34.6 L0 52 L0 17.3 Z'/%3E%3Cpath d='M30 0 L30 34.6'/%3E%3Cpath d='M0 17.3 L30 34.6 L60 17.3'/%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'bubbles',
        nameKey: 'lockScreen.patternBubbles',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cg stroke='%23ffffff' stroke-width='0.5' fill='none'%3E%3Ccircle cx='25' cy='25' r='20'/%3E%3Ccircle cx='75' cy='35' r='15'/%3E%3Ccircle cx='50' cy='75' r='22'/%3E%3Ccircle cx='85' cy='80' r='10'/%3E%3Ccircle cx='10' cy='70' r='8'/%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'none',
        nameKey: 'lockScreen.patternNone',
        svg: '',
    },
];

const LOCK_PATTERN_KEY = 'aeroftp_lock_pattern';
const DEFAULT_PATTERN = 'isometric';

interface LockScreenProps {
    onUnlock: () => void;
}

export const LockScreen: React.FC<LockScreenProps> = ({ onUnlock }) => {
    const t = useTranslation();
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [error, setError] = useState('');
    const [isLoading, setIsLoading] = useState(false);
    const [unlockStep, setUnlockStep] = useState(0);
    const [appVersion, setAppVersion] = useState('');

    useEffect(() => { getVersion().then(setAppVersion).catch(() => {}); }, []);
    const stepTimer = useRef<ReturnType<typeof setInterval> | null>(null);

    // Cryptographic unlock steps — real terms matching the actual vault unlock flow
    const unlockSteps = [
        t('lockScreen.stepDeriving'),      // "Deriving key (Argon2id)..."
        t('lockScreen.stepDecrypting'),    // "Decrypting passphrase..."
        t('lockScreen.stepExpanding'),     // "Expanding vault key (HKDF)..."
        t('lockScreen.stepVerifying'),     // "Verifying integrity..."
        t('lockScreen.stepLoading'),       // "Loading credentials..."
    ];

    // Step durations: longer pauses for crypto steps, short for final loading
    const stepDurations = [1300, 1300, 1300, 1300, 600];

    useEffect(() => {
        if (isLoading) {
            setUnlockStep(0);
            let currentStep = 0;
            const scheduleNext = () => {
                if (currentStep >= unlockSteps.length - 1) return;
                stepTimer.current = setTimeout(() => {
                    currentStep++;
                    setUnlockStep(currentStep);
                    scheduleNext();
                }, stepDurations[currentStep]);
            };
            scheduleNext();
        } else {
            if (stepTimer.current) clearTimeout(stepTimer.current);
            stepTimer.current = null;
        }
        return () => { if (stepTimer.current) clearTimeout(stepTimer.current); };
    }, [isLoading]);

    // Read pattern preference
    const patternId = localStorage.getItem(LOCK_PATTERN_KEY) || DEFAULT_PATTERN;
    const pattern = LOCK_SCREEN_PATTERNS.find(p => p.id === patternId) || LOCK_SCREEN_PATTERNS[0];

    // Focus password input on mount
    useEffect(() => {
        const input = document.getElementById('lock-password-input');
        input?.focus();
    }, []);

    const handleUnlock = async (e: React.FormEvent) => {
        e.preventDefault();
        if (!password || isLoading) return;

        setIsLoading(true);
        setError('');

        try {
            await invoke('unlock_credential_store', { password });
            onUnlock();
        } catch (err) {
            setError(t('lockScreen.invalidPassword'));
            setPassword('');
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <div className="fixed inset-0 z-[100] flex items-center justify-center bg-gradient-to-br from-gray-900 via-gray-800 to-gray-900">
            {/* Background pattern overlay */}
            {pattern.svg && (
                <div className="absolute inset-0 opacity-[0.04]">
                    <div className="absolute inset-0" style={{ backgroundImage: pattern.svg }} />
                </div>
            )}

            {/* Card — AeroVault modal style */}
            <div className="relative w-full max-w-md mx-4">
                <div className="bg-gray-800 rounded-lg shadow-2xl border border-gray-700 overflow-hidden">
                    {/* Header — icon + title (matches AeroVault) */}
                    <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-700">
                        <Shield size={18} className="text-emerald-400" />
                        <span className="font-medium text-gray-100">AeroFTP</span>
                        <span className="text-xs text-gray-500 ml-auto">{t('lockScreen.locked')}</span>
                    </div>

                    {/* Shield icon + form */}
                    <div className="p-6">
                        {/* Centered shield with lock badge */}
                        <div className="flex justify-center mb-5">
                            <div className="relative">
                                <Shield size={52} className="text-emerald-400" />
                                <div className="absolute -bottom-1 -right-1 bg-emerald-500 rounded-full p-1">
                                    <Lock size={11} className="text-white" />
                                </div>
                            </div>
                        </div>

                        {/* Error */}
                        {error && (
                            <div className="flex items-center gap-2 p-3 mb-4 bg-red-900/20 border border-red-800 rounded-lg text-red-400 text-sm">
                                <AlertCircle size={16} className="flex-shrink-0" />
                                {error}
                            </div>
                        )}

                        {/* Password form */}
                        <form onSubmit={handleUnlock} className="space-y-4">
                            <div>
                                <label htmlFor="lock-password-input" className="block text-sm font-medium text-gray-300 mb-2">
                                    {t('lockScreen.enterPassword')}
                                </label>
                                <div className="relative">
                                    <input
                                        id="lock-password-input"
                                        type={showPassword ? 'text' : 'password'}
                                        value={password}
                                        onChange={e => setPassword(e.target.value)}
                                        className="w-full px-4 py-3 pr-12 bg-gray-900 border border-gray-600 rounded-lg text-gray-100 focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-all placeholder-gray-500"
                                        placeholder="••••••••"
                                        autoComplete="current-password"
                                        disabled={isLoading}
                                    />
                                    <button
                                        type="button"
                                        onClick={() => setShowPassword(!showPassword)}
                                        className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-300 transition-colors"
                                        tabIndex={-1}
                                    >
                                        {showPassword ? <EyeOff size={20} /> : <Eye size={20} />}
                                    </button>
                                </div>
                            </div>

                            <button
                                type="submit"
                                disabled={!password || isLoading}
                                className="w-full py-3 bg-emerald-600 hover:bg-emerald-500 disabled:bg-gray-600 disabled:cursor-not-allowed text-white font-medium rounded-lg transition-colors flex items-center justify-center gap-2"
                            >
                                {isLoading ? (
                                    <>
                                        <svg className="h-5 w-5" viewBox="0 0 100 100" fill="currentColor">
                                            <path d="M31.6,3.5C5.9,13.6-6.6,42.7,3.5,68.4c10.1,25.7,39.2,38.3,64.9,28.1l-3.1-7.9c-21.3,8.4-45.4-2-53.8-23.3c-8.4-21.3,2-45.4,23.3-53.8L31.6,3.5z">
                                                <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                            </path>
                                            <path d="M42.3,39.6c5.7-4.3,13.9-3.1,18.1,2.7c4.3,5.7,3.1,13.9-2.7,18.1l4.1,5.5c8.8-6.5,10.6-19,4.1-27.7c-6.5-8.8-19-10.6-27.7-4.1L42.3,39.6z">
                                                <animateTransform attributeName="transform" type="rotate" dur="1s" from="0 50 50" to="-360 50 50" repeatCount="indefinite" />
                                            </path>
                                            <path d="M82,35.7C74.1,18,53.4,10.1,35.7,18S10.1,46.6,18,64.3l7.6-3.4c-6-13.5,0-29.3,13.5-35.3s29.3,0,35.3,13.5L82,35.7z">
                                                <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                            </path>
                                        </svg>
                                        <span className="transition-opacity duration-200">{unlockSteps[unlockStep]}</span>
                                    </>
                                ) : (
                                    <>
                                        <ShieldCheck size={20} />
                                        {t('lockScreen.unlock')}
                                    </>
                                )}
                            </button>
                        </form>
                    </div>

                    {/* Footer */}
                    <div className="px-6 pb-4">
                        <p className="text-xs text-center text-gray-500">
                            {t('lockScreen.securityNote')}
                        </p>
                    </div>
                </div>

                {/* Version badge */}
                <div className="mt-3 text-center">
                    <span className="text-xs text-gray-600">AeroFTP {appVersion ? `v${appVersion}` : ''}</span>
                </div>
            </div>
        </div>
    );
};
