/**
 * GlobalTooltip — intercepts all [title] attributes in the DOM and replaces
 * the native browser tooltip with a smooth animated one.
 *
 * Drop-in: mount once in App.tsx, no changes needed to existing components.
 */

import { useEffect, useRef } from 'react';

const SHOW_DELAY = 400; // ms before showing
const HIDE_DELAY = 80;  // ms before hiding after mouseleave

export const GlobalTooltip: React.FC = () => {
    const tooltipRef = useRef<HTMLDivElement | null>(null);
    const showTimer = useRef<number>(0);
    const hideTimer = useRef<number>(0);
    const currentTarget = useRef<HTMLElement | null>(null);
    const storedTitle = useRef<string>('');

    useEffect(() => {
        // Create tooltip element once
        const tip = document.createElement('div');
        tip.className = 'aero-tooltip';
        tip.setAttribute('role', 'tooltip');
        document.body.appendChild(tip);
        tooltipRef.current = tip;

        const show = (target: HTMLElement) => {
            const title = storedTitle.current;
            if (!title || !title.trim()) return;

            const tip = tooltipRef.current!;
            tip.textContent = title;

            // Position below the element
            const rect = target.getBoundingClientRect();
            const tipWidth = tip.offsetWidth || 120;
            let left = rect.left + rect.width / 2 - tipWidth / 2;
            let top = rect.bottom + 6;

            // Clamp to viewport
            const vw = window.innerWidth;
            const vh = window.innerHeight;
            if (left < 6) left = 6;
            if (left + tipWidth > vw - 6) left = vw - tipWidth - 6;

            // Flip above if near bottom
            if (top + 30 > vh) {
                top = rect.top - 30 - 6;
            }

            tip.style.left = `${left}px`;
            tip.style.top = `${top}px`;
            tip.classList.add('visible');
        };

        const hide = () => {
            tooltipRef.current?.classList.remove('visible');
            // Restore title
            if (currentTarget.current && storedTitle.current) {
                currentTarget.current.setAttribute('title', storedTitle.current);
            }
            currentTarget.current = null;
            storedTitle.current = '';
        };

        const onMouseEnter = (e: MouseEvent) => {
            const target = (e.target as HTMLElement).closest?.('[title]') as HTMLElement | null;
            if (!target || !target.getAttribute('title')) return;

            // Restore previous element's title if switching targets
            if (currentTarget.current && currentTarget.current !== target && storedTitle.current) {
                currentTarget.current.setAttribute('title', storedTitle.current);
            }

            // Immediately strip native title to prevent browser tooltip
            storedTitle.current = target.getAttribute('title')!;
            target.removeAttribute('title');
            currentTarget.current = target;

            clearTimeout(hideTimer.current);
            clearTimeout(showTimer.current);
            showTimer.current = window.setTimeout(() => show(target), SHOW_DELAY);
        };

        const onMouseLeave = (e: MouseEvent) => {
            const related = e.relatedTarget as HTMLElement | null;

            // If moving to a child of the current tooltip target, stay open
            if (currentTarget.current && related && currentTarget.current.contains(related)) return;

            // If leaving to another [title] element, let mouseenter handle it
            if (related?.closest?.('[title]')?.getAttribute('title')) return;

            if (currentTarget.current) {
                clearTimeout(showTimer.current);
                hideTimer.current = window.setTimeout(hide, HIDE_DELAY);
            }
        };

        const onMouseDown = () => {
            clearTimeout(showTimer.current);
            hide();
        };

        const onScroll = () => {
            clearTimeout(showTimer.current);
            hide();
        };

        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') {
                clearTimeout(showTimer.current);
                hide();
            }
        };

        document.addEventListener('mouseover', onMouseEnter, true);
        document.addEventListener('mouseout', onMouseLeave, true);
        document.addEventListener('mousedown', onMouseDown, true);
        document.addEventListener('scroll', onScroll, true);
        document.addEventListener('keydown', onKeyDown, true);

        return () => {
            document.removeEventListener('mouseover', onMouseEnter, true);
            document.removeEventListener('mouseout', onMouseLeave, true);
            document.removeEventListener('mousedown', onMouseDown, true);
            document.removeEventListener('scroll', onScroll, true);
            document.removeEventListener('keydown', onKeyDown, true);
            clearTimeout(showTimer.current);
            clearTimeout(hideTimer.current);
            hide();
            tip.remove();
        };
    }, []);

    return null; // Renderless component
};

export default GlobalTooltip;
