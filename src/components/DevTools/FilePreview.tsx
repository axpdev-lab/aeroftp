import React, { useEffect, useMemo } from 'react';
import { useTranslation } from '../../i18n';
import Prism from 'prismjs';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { FileText, Image as ImageIcon, FileCode } from 'lucide-react';
import { PreviewFile, getFileLanguage, isImageFile, isMarkdownFile } from './types';

// Import Prism core languages first (required for PHP)
import 'prismjs/components/prism-markup';
import 'prismjs/components/prism-clike';
import 'prismjs/components/prism-markup-templating';

// Import other Prism languages
import 'prismjs/components/prism-javascript';
import 'prismjs/components/prism-typescript';
import 'prismjs/components/prism-jsx';
import 'prismjs/components/prism-tsx';
import 'prismjs/components/prism-css';
import 'prismjs/components/prism-scss';
import 'prismjs/components/prism-json';
import 'prismjs/components/prism-markdown';
import 'prismjs/components/prism-php';
import 'prismjs/components/prism-python';
import 'prismjs/components/prism-bash';
import 'prismjs/components/prism-sql';
import 'prismjs/components/prism-yaml';
import 'prismjs/components/prism-rust';

// Prism theme
import 'prismjs/themes/prism-tomorrow.css';

interface FilePreviewProps {
    file: PreviewFile | null;
    className?: string;
}

export const FilePreview: React.FC<FilePreviewProps> = ({ file, className = '' }) => {
    const t = useTranslation();
    // Trigger Prism highlighting when content changes
    useEffect(() => {
        if (file && !isImageFile(file.name) && !isMarkdownFile(file.name)) {
            Prism.highlightAll();
        }
    }, [file]);

    const language = useMemo(() => file ? getFileLanguage(file.name) : 'text', [file]);

    if (!file) {
        return (
            <div className={`flex flex-col items-center justify-center h-full text-gray-400 ${className}`}>
                <FileText size={48} className="mb-3 opacity-30" />
                <p>{t('preview.noFileSelected')}</p>
                <p className="text-sm mt-1">{t('devtools.previewPanel.emptyState')}</p>
            </div>
        );
    }

    // Image preview
    if (isImageFile(file.name)) {
        // For remote files, we'd need to fetch as base64
        // For now, show placeholder or local file path
        return (
            <div className={`flex flex-col items-center justify-center h-full ${className}`}>
                <div className="bg-gray-100 dark:bg-gray-700 rounded-lg p-4 flex flex-col items-center">
                    <ImageIcon size={32} className="text-blue-500 mb-2" />
                    <p className="font-medium">{file.name}</p>
                    <p className="text-sm text-gray-500">{(file.size / 1024).toFixed(1)} KB</p>
                    {file.content && (
                        <img
                            src={file.content.startsWith('data:') ? file.content : `data:image/${file.name.split('.').pop()};base64,${file.content}`}
                            alt={file.name}
                            className="mt-4 max-w-full max-h-96 rounded-lg shadow-lg"
                        />
                    )}
                </div>
            </div>
        );
    }

    // Markdown preview
    if (isMarkdownFile(file.name)) {
        return (
            <div className={`h-full overflow-auto ${className}`}>
                <div className="p-4 prose dark:prose-invert max-w-none">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>
                        {file.content}
                    </ReactMarkdown>
                </div>
            </div>
        );
    }

    // Code preview with syntax highlighting
    return (
        <div className={`h-full overflow-auto ${className}`}>
            {/* File info header */}
            <div className="sticky top-0 bg-gray-800 border-b border-gray-700 px-4 py-2 flex items-center gap-2 text-sm text-gray-300">
                <FileCode size={14} />
                <span className="font-medium">{file.name}</span>
                <span className="text-gray-500">•</span>
                <span className="text-gray-500">{language}</span>
                <span className="text-gray-500">•</span>
                <span className="text-gray-500">{(file.size / 1024).toFixed(1)} KB</span>
                {file.isRemote && (
                    <>
                        <span className="text-gray-500">•</span>
                        <span className="text-blue-400">Remote</span>
                    </>
                )}
            </div>

            {/* Code with line numbers */}
            <div className="relative">
                <pre className="!m-0 !rounded-none !bg-gray-900 text-sm overflow-x-auto">
                    <code className={`language-${language}`}>
                        {file.content}
                    </code>
                </pre>
            </div>
        </div>
    );
};

export default FilePreview;
