import * as React from 'react';

interface ImportExportIconProps {
    size?: number;
    className?: string;
}

export const ImportExportIcon: React.FC<ImportExportIconProps> = ({ size = 16, className = '' }) => (
    <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 16 16"
        width={size}
        height={size}
        className={className}
        fill="currentColor"
    >
        <path d="M14 12v2H2v-2H1v2l0.0038 -0.00245A0.99885 0.99885 0 0 0 2 15h12a1 1 0 0 0 1 -1v-2Z" />
        <path d="M13.8 7.3 12 9.1 12 2l-1 0 0 7.1 -1.8 -1.8L8.5 8l3 3 3 -3 -0.7 -0.7z" />
        <path d="m4.5 2 -3 3 0.7 0.7L4 3.9 4 11l1 0 0 -7.1 1.8 1.8L7.5 5 4.5 2z" />
    </svg>
);

export default ImportExportIcon;
