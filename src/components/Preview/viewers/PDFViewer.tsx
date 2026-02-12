/**
 * PDF Viewer Component
 * 
 * Since Tauri WebView doesn't support inline PDF rendering,
 * we offer download option.
 */

import React, { useState } from 'react';
import { Download, FileText, ExternalLink } from 'lucide-react';
import { ViewerBaseProps } from '../types';
import { useI18n } from '../../../i18n';

interface PDFViewerProps extends ViewerBaseProps {
    className?: string;
}

export const PDFViewer: React.FC<PDFViewerProps> = ({
    file,
    className = '',
}) => {
    const { t } = useI18n();
    const [downloading, setDownloading] = useState(false);

    // PDF source URL
    const pdfSrc = file.blobUrl || file.content as string || '';

    // Download PDF and open
    const downloadAndOpen = async () => {
        if (!pdfSrc) return;

        setDownloading(true);
        try {
            // Create a link and click to download
            const link = document.createElement('a');
            link.href = pdfSrc;
            link.download = file.name;
            document.body.appendChild(link);
            link.click();
            document.body.removeChild(link);

            // Also try to open in new tab
            window.open(pdfSrc, '_blank');
        } finally {
            setDownloading(false);
        }
    };

    // Just download
    const downloadPdf = () => {
        if (pdfSrc) {
            const link = document.createElement('a');
            link.href = pdfSrc;
            link.download = file.name;
            link.click();
        }
    };

    return (
        <div className={`flex flex-col h-full bg-gray-800 ${className}`}>
            {/* Header */}
            <div className="flex items-center gap-2 px-4 py-3 bg-gray-700 border-b border-gray-600">
                <FileText size={18} className="text-red-400" />
                <span className="text-sm font-medium text-gray-200 truncate">{file.name}</span>
                {file.size && (
                    <span className="text-xs text-gray-500 ml-2">
                        {(file.size / 1024 / 1024).toFixed(2)} MB
                    </span>
                )}
            </div>

            {/* Content */}
            <div className="flex-1 flex flex-col items-center justify-center p-8">
                <div className="text-center mb-8">
                    <FileText size={80} className="mx-auto mb-4 text-red-400/50" />
                    <h3 className="text-xl font-medium text-gray-200 mb-2">PDF Document</h3>
                    <p className="text-gray-400 text-sm max-w-md">
                        {t('preview.pdf.downloadMessage')}
                    </p>
                </div>

                <div className="flex flex-col gap-3 w-full max-w-xs">
                    {/* Open in new tab */}
                    <button
                        onClick={downloadAndOpen}
                        disabled={downloading || !pdfSrc}
                        className="w-full px-6 py-3 bg-blue-500 hover:bg-blue-600 disabled:bg-blue-500/50 text-white rounded-xl font-medium flex items-center justify-center gap-2 transition-colors shadow-lg shadow-blue-500/20"
                    >
                        <ExternalLink size={20} />
                        {t('preview.pdf.open')}
                    </button>

                    {/* Download button */}
                    <button
                        onClick={downloadPdf}
                        disabled={!pdfSrc}
                        className="w-full px-6 py-3 bg-gray-600 hover:bg-gray-500 text-white rounded-xl font-medium flex items-center justify-center gap-2 transition-colors"
                    >
                        <Download size={20} />
                        {t('preview.pdf.download')}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default PDFViewer;
