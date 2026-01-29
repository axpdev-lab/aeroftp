import * as React from 'react';
import { useState, useEffect, useRef, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { ChevronRight } from 'lucide-react';

export interface ContextMenuItem {
    label: string;
    icon: React.ReactNode;
    action: () => void;
    disabled?: boolean;
    danger?: boolean;
    divider?: boolean;
    /** Submenu items - renders a hover-expandable nested menu */
    children?: ContextMenuItem[];
}

export interface ContextMenuProps {
    x: number;
    y: number;
    items: ContextMenuItem[];
    onClose: () => void;
}

export const ContextMenu: React.FC<ContextMenuProps> = ({ x, y, items, onClose }) => {
    const menuRef = useRef<HTMLDivElement>(null);
    const submenuRef = useRef<HTMLDivElement>(null);
    const [isVisible, setIsVisible] = useState(false);
    const [activeSubmenu, setActiveSubmenu] = useState<number | null>(null);
    const [submenuParentRect, setSubmenuParentRect] = useState<DOMRect | null>(null);
    const [submenuPos, setSubmenuPos] = useState({ left: 0, top: 0 });
    const closeTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Smooth entrance animation
    useEffect(() => {
        requestAnimationFrame(() => setIsVisible(true));
    }, []);

    // Click outside / Escape handling - check both menu and submenu refs
    useEffect(() => {
        const handleClickOutside = (e: MouseEvent) => {
            const target = e.target as Node;
            const inMenu = menuRef.current?.contains(target);
            const inSubmenu = submenuRef.current?.contains(target);
            if (!inMenu && !inSubmenu) {
                onClose();
            }
        };
        const handleEscape = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('mousedown', handleClickOutside);
        document.addEventListener('keydown', handleEscape);
        return () => {
            document.removeEventListener('mousedown', handleClickOutside);
            document.removeEventListener('keydown', handleEscape);
        };
    }, [onClose]);

    // Cleanup timeout on unmount
    useEffect(() => {
        return () => { if (closeTimeoutRef.current) clearTimeout(closeTimeoutRef.current); };
    }, []);

    // Calculate submenu position when parent rect changes
    useEffect(() => {
        if (!submenuParentRect || activeSubmenu === null) return;
        const children = items[activeSubmenu]?.children;
        if (!children) return;

        const menuWidth = 200;
        const menuHeight = children.length * 34;
        const padding = 10;

        let left = submenuParentRect.right + 2;
        let top = submenuParentRect.top;

        if (left + menuWidth > window.innerWidth - padding) {
            left = submenuParentRect.left - menuWidth - 2;
        }
        if (top + menuHeight > window.innerHeight - padding) {
            top = window.innerHeight - menuHeight - padding;
        }
        if (top < padding) top = padding;

        setSubmenuPos({ left, top });
    }, [submenuParentRect, activeSubmenu, items]);

    const cancelClose = useCallback(() => {
        if (closeTimeoutRef.current) {
            clearTimeout(closeTimeoutRef.current);
            closeTimeoutRef.current = null;
        }
    }, []);

    const scheduleClose = useCallback(() => {
        cancelClose();
        closeTimeoutRef.current = setTimeout(() => {
            setActiveSubmenu(null);
            setSubmenuParentRect(null);
        }, 200);
    }, [cancelClose]);

    const handleItemMouseEnter = useCallback((index: number, e: React.MouseEvent<HTMLButtonElement>) => {
        cancelClose();
        const item = items[index];
        if (item.children && item.children.length > 0) {
            const rect = e.currentTarget.getBoundingClientRect();
            setActiveSubmenu(index);
            setSubmenuParentRect(rect);
        } else {
            scheduleClose();
        }
    }, [items, cancelClose, scheduleClose]);

    // Adjust position to not overflow viewport
    const adjustedPosition = () => {
        const menuWidth = 200;
        const menuHeight = items.length * 38;
        const padding = 10;
        let adjustedX = x;
        let adjustedY = y;
        if (x + menuWidth > window.innerWidth - padding) {
            adjustedX = window.innerWidth - menuWidth - padding;
        }
        if (y + menuHeight > window.innerHeight - padding) {
            adjustedY = window.innerHeight - menuHeight - padding;
        }
        return { left: adjustedX, top: adjustedY };
    };

    const pos = adjustedPosition();
    const activeChildren = activeSubmenu !== null ? items[activeSubmenu]?.children : null;

    return (
        <>
            {/* Main menu */}
            <div
                ref={menuRef}
                className={`
                    fixed z-50 bg-white/95 dark:bg-gray-800/95 backdrop-blur-lg
                    rounded-xl shadow-2xl border border-gray-200/50 dark:border-gray-700/50
                    py-1 min-w-[180px] overflow-hidden
                    transition-all duration-150 ease-out
                    ${isVisible
                        ? 'opacity-100 scale-100 translate-y-0'
                        : 'opacity-0 scale-95 -translate-y-1'}
                `}
                style={{ left: pos.left, top: pos.top }}
                onMouseLeave={scheduleClose}
                onMouseEnter={cancelClose}
            >
                {items.map((item, index) => (
                    <React.Fragment key={index}>
                        {item.divider && index > 0 && (
                            <div className="h-px bg-gray-200/80 dark:bg-gray-700/80 my-1 mx-2" />
                        )}
                        <button
                            onMouseEnter={(e) => handleItemMouseEnter(index, e)}
                            onClick={() => {
                                if (!item.disabled && !item.children) {
                                    item.action();
                                    onClose();
                                }
                            }}
                            disabled={item.disabled}
                            className={`
                                w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2.5
                                transition-all duration-100
                                ${activeSubmenu === index ? 'bg-gray-100/80 dark:bg-gray-700/80' : ''}
                                ${item.disabled
                                    ? 'text-gray-400 cursor-not-allowed'
                                    : item.danger
                                        ? 'text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/40'
                                        : 'text-gray-700 dark:text-gray-200 hover:bg-gray-100/80 dark:hover:bg-gray-700/80'
                                }
                            `}
                        >
                            <span className="w-4 h-4 flex items-center justify-center opacity-70">
                                {item.icon}
                            </span>
                            <span className="font-medium flex-1">{item.label}</span>
                            {item.children && item.children.length > 0 && (
                                <ChevronRight size={14} className="opacity-50" />
                            )}
                        </button>
                    </React.Fragment>
                ))}
            </div>

            {/* Submenu - rendered as sibling via portal to avoid mouseLeave issues */}
            {activeChildren && activeChildren.length > 0 && createPortal(
                <div
                    ref={submenuRef}
                    className="fixed z-[60] bg-white/95 dark:bg-gray-800/95 backdrop-blur-lg
                        rounded-xl shadow-2xl border border-gray-200/50 dark:border-gray-700/50
                        py-1 min-w-[180px] overflow-hidden
                        transition-opacity duration-100 opacity-100"
                    style={{ left: submenuPos.left, top: submenuPos.top }}
                    onMouseEnter={cancelClose}
                    onMouseLeave={scheduleClose}
                >
                    {activeChildren.map((item, index) => (
                        <React.Fragment key={index}>
                            {item.divider && index > 0 && (
                                <div className="h-px bg-gray-200/80 dark:bg-gray-700/80 my-1 mx-2" />
                            )}
                            <button
                                onClick={() => {
                                    if (!item.disabled) {
                                        item.action();
                                        onClose();
                                    }
                                }}
                                disabled={item.disabled}
                                className={`
                                    w-full px-3 py-1.5 text-left text-[13px] flex items-center gap-2.5
                                    transition-all duration-100
                                    ${item.disabled
                                        ? 'text-gray-400 cursor-not-allowed'
                                        : item.danger
                                            ? 'text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/40'
                                            : 'text-gray-700 dark:text-gray-200 hover:bg-gray-100/80 dark:hover:bg-gray-700/80'
                                    }
                                `}
                            >
                                <span className="w-4 h-4 flex items-center justify-center opacity-70">
                                    {item.icon}
                                </span>
                                <span className="font-medium">{item.label}</span>
                            </button>
                        </React.Fragment>
                    ))}
                </div>,
                document.body
            )}
        </>
    );
};

// Hook for managing context menu
export interface ContextMenuState {
    visible: boolean;
    x: number;
    y: number;
    items: ContextMenuItem[];
}

export const useContextMenu = () => {
    const [state, setState] = useState<ContextMenuState>({
        visible: false,
        x: 0,
        y: 0,
        items: [],
    });

    const show = (e: React.MouseEvent, items: ContextMenuItem[]) => {
        e.preventDefault();
        e.stopPropagation();
        setState({
            visible: true,
            x: e.clientX,
            y: e.clientY,
            items,
        });
    };

    const hide = () => {
        setState(prev => ({ ...prev, visible: false }));
    };

    return { state, show, hide };
};

export default ContextMenu;
