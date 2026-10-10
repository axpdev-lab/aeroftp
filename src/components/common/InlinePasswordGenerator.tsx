// SPDX-License-Identifier: GPL-3.0-or-later

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Check, Loader2, WandSparkles } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { PASSWORD_PRESETS, passwordCommandArgs, type PasswordPresetId } from '../../utils/passwordForge';

interface InlinePasswordGeneratorProps {
    onGenerated: (password: string) => void;
    preset?: PasswordPresetId;
    disabled?: boolean;
    className?: string;
}

export const InlinePasswordGenerator: React.FC<InlinePasswordGeneratorProps> = ({
    onGenerated,
    preset = 'balanced',
    disabled = false,
    className = '',
}) => {
    const t = useTranslation();
    const [status, setStatus] = useState<'idle' | 'loading' | 'done'>('idle');
    const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
    const buttonRef = useRef<HTMLButtonElement | null>(null);

    useEffect(() => () => {
        if (timerRef.current) clearTimeout(timerRef.current);
    }, []);

    const generate = useCallback(async () => {
        if (disabled || status === 'loading') return;
        setStatus('loading');
        try {
            const values = await invoke<string[]>('generate_password', passwordCommandArgs(PASSWORD_PRESETS[preset]));
            if (values[0]) {
                onGenerated(values[0]);
                setStatus('done');
                if (timerRef.current) clearTimeout(timerRef.current);
                timerRef.current = setTimeout(() => setStatus('idle'), 1600);
            } else {
                setStatus('idle');
            }
        } catch {
            setStatus('idle');
        }
    }, [disabled, onGenerated, preset, status]);

    // The button is out of the Tab order (below), so the field it sits in
    // gets the keyboard way to it: Alt+G while typing there generates.
    useEffect(() => {
        const field = buttonRef.current?.parentElement?.querySelector('input');
        if (!field) return undefined;
        const onKey = (e: KeyboardEvent) => {
            if (e.altKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === 'g') {
                e.preventDefault();
                void generate();
            }
        };
        field.addEventListener('keydown', onKey);
        return () => field.removeEventListener('keydown', onKey);
    }, [generate]);

    const title = `${preset === 'compatible'
        ? t('cyberTools.pwdGenerateCompatibleInline')
        : t('cyberTools.pwdGenerateInline')} (Alt+G)`;

    return (
        // Out of the Tab order, like the show-password eye beside it: Tab from
        // a password field goes on to the next field, not to the icons inside
        // this one. Keyboard users generate with Alt+G from the field.
        <button
            ref={buttonRef}
            type="button"
            tabIndex={-1}
            aria-keyshortcuts="Alt+G"
            onClick={generate}
            disabled={disabled || status === 'loading'}
            className={`inline-flex h-7 w-7 items-center justify-center rounded-md text-gray-400 transition-all hover:bg-cyan-500/10 hover:text-cyan-600 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-cyan-500/60 disabled:cursor-not-allowed disabled:opacity-40 dark:hover:text-cyan-300 ${className}`}
            title={title}
            aria-label={title}
        >
            {status === 'loading' && <Loader2 size={15} className="animate-spin" />}
            {status === 'done' && <Check size={15} className="animate-scale-in text-emerald-500" />}
            {status === 'idle' && <WandSparkles size={15} />}
        </button>
    );
};

export default InlinePasswordGenerator;
