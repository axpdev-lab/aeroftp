import React, { useEffect, useRef, useState } from 'react';

// Scramble charset: hex + a few light crypto-ish glyphs, so the in-flight text
// reads as "ciphertext being decrypted" without the block elements (░▒▓█) that
// made it look noisy.
const GLYPHS = '0123456789ABCDEF/<>=+*?#%&';

// Cap the scramble width so a long encrypted blob name (40+ chars) doesn't wrap
// to a second line during the wait; the reveal still uses the full final text.
// Kept generous (the name column has room) while `truncate` clips narrow cols.
const MAX_SCRAMBLE = 30;

const randGlyph = (seed: number) => GLYPHS[(seed * 2654435761) % GLYPHS.length];

interface DecryptingTextProps {
    /** Final (decrypted) text to resolve to. */
    text: string;
    /** While true: continuous scramble ("decrypting…"). When it flips to false,
     *  the text resolves to `text` character by character (the reveal "puff"). */
    active: boolean;
    className?: string;
    /** Reveal duration once `active` turns false. */
    durationMs?: number;
    /** Per-row stagger so rows resolve in a cascade, not all at once. */
    delayMs?: number;
    title?: string;
}

/**
 * Text decryption animation. Used on remote file names while an AeroCrypt
 * overlay is being unlocked: the names scramble (you can tell something is
 * happening), then resolve to the decrypted names. Respects
 * prefers-reduced-motion (renders the final text directly).
 */
export const DecryptingText: React.FC<DecryptingTextProps> = ({
    text,
    active,
    className,
    durationMs = 700,
    delayMs = 0,
    title,
}) => {
    const reduceMotion = typeof window !== 'undefined'
        && window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;

    const [display, setDisplay] = useState(text);
    const frameRef = useRef<number | null>(null);
    const tickRef = useRef(0);

    useEffect(() => {
        if (reduceMotion) { setDisplay(text); return; }

        // Phase 1 — continuous scramble while decrypting.
        if (active) {
            let raf = 0;
            const len = Math.min(text.length || MAX_SCRAMBLE, MAX_SCRAMBLE);
            const loop = () => {
                tickRef.current += 1;
                const t = tickRef.current;
                const out = Array.from({ length: len }, (_, i) => randGlyph(t + i * 7)).join('');
                setDisplay(out);
                raf = requestAnimationFrame(() => {
                    // calm shimmer (~10 fps): fast/dense scrambling read as chaotic
                    frameRef.current = window.setTimeout(loop, 100) as unknown as number;
                });
            };
            loop();
            return () => {
                if (frameRef.current) clearTimeout(frameRef.current);
                cancelAnimationFrame(raf);
            };
        }

        // Phase 2 — resolve to the final text, left-to-right, after delayMs.
        const start = performance.now() + delayMs;
        const total = Math.max(120, durationMs);
        let raf = 0;
        const step = (now: number) => {
            const elapsed = now - start;
            if (elapsed < 0) { raf = requestAnimationFrame(step); return; }
            const progress = Math.min(1, elapsed / total);
            const revealed = Math.floor(progress * text.length);
            tickRef.current += 1;
            const out = Array.from(text, (ch, i) =>
                i < revealed ? ch : randGlyph(tickRef.current + i * 13),
            ).join('');
            setDisplay(out);
            if (progress < 1) {
                raf = requestAnimationFrame(step);
            } else {
                setDisplay(text);
            }
        };
        raf = requestAnimationFrame(step);
        return () => cancelAnimationFrame(raf);
    }, [text, active, durationMs, delayMs, reduceMotion]);

    return (
        <span
            className={`relative inline-block max-w-full truncate align-bottom overflow-hidden ${active ? 'font-mono' : ''} ${className ?? ''}`}
            title={title ?? text}
        >
            {display}
        </span>
    );
};

export default DecryptingText;
