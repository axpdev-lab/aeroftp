import * as React from 'react';
import { useState, useEffect, useCallback, useRef } from 'react';

export type ToastType = 'success' | 'error' | 'warning' | 'info';

export interface ToastProps {
    id: string;
    type: ToastType;
    title: string;
    message?: string;
    duration?: number;
    onClose: (id: string) => void;
}

const toastStyles: Record<ToastType, { bg: string; icon: string; border: string }> = {
    success: {
        bg: 'bg-green-50 dark:bg-green-900/30',
        border: 'border-green-200 dark:border-green-800',
        icon: '✓',
    },
    error: {
        bg: 'bg-red-50 dark:bg-red-900/30',
        border: 'border-red-200 dark:border-red-800',
        icon: '✕',
    },
    warning: {
        bg: 'bg-yellow-50 dark:bg-yellow-900/30',
        border: 'border-yellow-200 dark:border-yellow-800',
        icon: '⚠',
    },
    info: {
        bg: 'bg-blue-50 dark:bg-blue-900/30',
        border: 'border-blue-200 dark:border-blue-800',
        icon: 'ℹ',
    },
};

const iconColors: Record<ToastType, string> = {
    success: 'text-green-500',
    error: 'text-red-500',
    warning: 'text-yellow-500',
    info: 'text-blue-500',
};

export const Toast: React.FC<ToastProps> = ({
    id,
    type,
    title,
    message,
    duration = 2000,
    onClose,
}: ToastProps) => {
    const [isExiting, setIsExiting] = useState(false);
    const onCloseRef = useRef(onClose);
    onCloseRef.current = onClose;

    useEffect(() => {
        if (duration > 0) {
            const timer = setTimeout(() => {
                setIsExiting(true);
                setTimeout(() => onCloseRef.current(id), 300);
            }, duration);
            return () => clearTimeout(timer);
        }
    }, [duration, id]);

    const handleClose = () => {
        setIsExiting(true);
        setTimeout(() => onClose(id), 300);
    };

    const styles = toastStyles[type];
    const iconColor = iconColors[type];

    return (
        <div
            className={`
        flex items-start gap-3 p-4 rounded-xl border shadow-lg backdrop-blur-sm
        ${styles.bg} ${styles.border}
        transform transition-all duration-300 ease-out
        ${isExiting ? 'opacity-0 translate-x-8' : 'opacity-100 translate-x-0'}
      `}
        >
            <span className={`text-xl ${iconColor}`}>{styles.icon}</span>
            <div className="flex-1 min-w-0">
                <p className="font-medium text-gray-900 dark:text-gray-100">{title}</p>
                {message && (
                    <p className="text-sm text-gray-600 dark:text-gray-400 mt-1">{message}</p>
                )}
            </div>
            <button
                onClick={handleClose}
                className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 transition-colors"
            >
                <svg className="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                </svg>
            </button>
        </div>
    );
};

// Toast Container Component
export interface ToastItem {
    id: string;
    type: ToastType;
    title: string;
    message?: string;
    duration?: number;
}

interface ToastContainerProps {
    toasts: ToastItem[];
    onRemove: (id: string) => void;
}

export const ToastContainer: React.FC<ToastContainerProps> = ({ toasts, onRemove }: ToastContainerProps) => {
    return (
        <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm">
            {toasts.map((toast: ToastItem) => (
                <Toast key={toast.id} {...toast} onClose={onRemove} />
            ))}
        </div>
    );
};

// Hook for managing toasts
let toastId = 0;

export const useToast = () => {
    const [toasts, setToasts] = useState<ToastItem[]>([]);

    const addToast = useCallback((type: ToastType, title: string, message?: string, duration?: number): string => {
        const id = `toast-${++toastId}`;
        setToasts((prev) => {
            const newToasts = [...prev, { id, type, title, message, duration }];
            // Limit to max 3 toasts to prevent accumulation
            return newToasts.slice(-3);
        });
        return id;
    }, []);

    const removeToast = useCallback((id: string): void => {
        setToasts((prev) => prev.filter((t) => t.id !== id));
    }, []);

    const success = useCallback((title: string, message?: string): string => addToast('success', title, message), [addToast]);
    const error = useCallback((title: string, message?: string): string => addToast('error', title, message, 3000), [addToast]);
    const warning = useCallback((title: string, message?: string): string => addToast('warning', title, message), [addToast]);
    const info = useCallback((title: string, message?: string): string => addToast('info', title, message), [addToast]);

    return {
        toasts,
        addToast,
        removeToast,
        success,
        error,
        warning,
        info,
    };
};

export default Toast;
