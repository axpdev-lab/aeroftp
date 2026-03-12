import * as React from 'react';
import { useTranslation } from '../../i18n';

interface PasswordStrengthBarProps {
    password: string;
}

type StrengthLevel = 0 | 1 | 2 | 3 | 4;

interface StrengthResult {
    level: StrengthLevel;
    score: number; // 0-100
}

/** Lightweight password strength calculator — no external deps */
function computeStrength(pw: string): StrengthResult {
    if (!pw) return { level: 0, score: 0 };

    let score = 0;

    // Length scoring (most important factor)
    score += Math.min(pw.length * 4, 40);

    // Character variety
    const hasLower = /[a-z]/.test(pw);
    const hasUpper = /[A-Z]/.test(pw);
    const hasDigit = /[0-9]/.test(pw);
    const hasSymbol = /[^a-zA-Z0-9]/.test(pw);
    const varietyCount = [hasLower, hasUpper, hasDigit, hasSymbol].filter(Boolean).length;
    score += varietyCount * 10;

    // Bonus for mixing character types
    if (varietyCount >= 3 && pw.length >= 12) score += 10;
    if (varietyCount >= 4 && pw.length >= 16) score += 10;

    // Penalty for repetition
    const uniqueChars = new Set(pw.toLowerCase()).size;
    if (uniqueChars < pw.length * 0.5) score -= 10;

    // Penalty for sequential characters (abc, 123)
    let sequential = 0;
    for (let i = 0; i < pw.length - 2; i++) {
        const c1 = pw.charCodeAt(i);
        const c2 = pw.charCodeAt(i + 1);
        const c3 = pw.charCodeAt(i + 2);
        if (c2 === c1 + 1 && c3 === c2 + 1) sequential++;
    }
    score -= sequential * 5;

    score = Math.max(0, Math.min(100, score));

    let level: StrengthLevel;
    if (score < 20) level = 0;
    else if (score < 40) level = 1;
    else if (score < 60) level = 2;
    else if (score < 80) level = 3;
    else level = 4;

    return { level, score };
}

const LEVEL_CONFIGS: Record<StrengthLevel, { color: string; bgColor: string; key: string }> = {
    0: { color: 'bg-gray-300 dark:bg-gray-600', bgColor: 'bg-gray-200 dark:bg-gray-700', key: 'vault.strength.none' },
    1: { color: 'bg-red-500', bgColor: 'bg-red-100 dark:bg-red-900/30', key: 'vault.strength.weak' },
    2: { color: 'bg-orange-500', bgColor: 'bg-orange-100 dark:bg-orange-900/30', key: 'vault.strength.fair' },
    3: { color: 'bg-emerald-500', bgColor: 'bg-emerald-100 dark:bg-emerald-900/30', key: 'vault.strength.strong' },
    4: { color: 'bg-blue-500', bgColor: 'bg-blue-100 dark:bg-blue-900/30', key: 'vault.strength.excellent' },
};

export const PasswordStrengthBar: React.FC<PasswordStrengthBarProps> = React.memo(({ password }) => {
    const t = useTranslation();
    const { level, score } = computeStrength(password);
    const config = LEVEL_CONFIGS[level];

    if (!password) return null;

    return (
        <div className="space-y-1">
            {/* Animated bar */}
            <div className="flex gap-1 h-1.5">
                {[1, 2, 3, 4].map((segment) => (
                    <div
                        key={segment}
                        className={`flex-1 rounded-full transition-all duration-500 ease-out ${
                            segment <= level ? config.color : 'bg-gray-200 dark:bg-gray-700'
                        }`}
                        style={{
                            transform: segment <= level ? 'scaleX(1)' : 'scaleX(0.85)',
                            opacity: segment <= level ? 1 : 0.4,
                            transitionDelay: `${segment * 50}ms`,
                        }}
                    />
                ))}
            </div>
            {/* Label */}
            <div className="flex items-center justify-between">
                <span className={`text-[10px] font-medium ${
                    level === 0 ? 'text-gray-400' :
                    level === 1 ? 'text-red-500' :
                    level === 2 ? 'text-orange-500' :
                    level === 3 ? 'text-emerald-500' :
                    'text-blue-500'
                }`}>
                    {t(config.key)}
                </span>
                <span className="text-[10px] text-gray-400">{score}/100</span>
            </div>
        </div>
    );
});
