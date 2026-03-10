import * as React from 'react';

interface VaultIconProps {
    size?: number;
    className?: string;
}

/**
 * AeroVault shield+lock icon — matches the OS MIME type icon (application-x-aerovault).
 * Emerald shield (#10b981) with white padlock, light green fill (#d4fbee).
 * Used in titlebar, VaultPanel, context menus, and file list.
 */
export const VaultIcon: React.FC<VaultIconProps> = ({ size = 24, className = '' }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 24 24"
        width={size}
        height={size}
        className={className}
        fill="none"
    >
        {/* Shield body */}
        <path
            d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z"
            fill="#d4fbee"
            stroke="#10b981"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
        />
        {/* Lock body */}
        <rect
            x="9.25"
            y="11"
            width="5.5"
            height="4"
            rx="0.75"
            fill="#fff"
            stroke="#10b981"
            strokeWidth="1.2"
            strokeLinecap="round"
            strokeLinejoin="round"
        />
        {/* Lock shackle */}
        <path
            d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11"
            fill="none"
            stroke="#10b981"
            strokeWidth="1.2"
            strokeLinecap="round"
            strokeLinejoin="round"
        />
    </svg>
);

export default VaultIcon;
