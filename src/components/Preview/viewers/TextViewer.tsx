/**
 * Text Viewer Component
 *
 * Simple text file viewer/editor.
 * Supports plain text, markdown, and code files.
 * Render toggle for HTML and Markdown preview.
 * Dev tools: responsive viewport, zoom, color picker, open in browser.
 */

import React, { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import { FileText, Copy, Check, Download, WrapText, Eye, Code2, Smartphone, Tablet, Monitor, ZoomIn, ZoomOut, ExternalLink, Pipette, RefreshCw } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { ViewerBaseProps } from '../types';
import { MarkdownRenderer } from '../../DevTools/MarkdownRenderer';
import Prism from 'prismjs';

interface TextViewerProps extends ViewerBaseProps {
    className?: string;
}

const isHTMLFile = (name: string) => /\.html?$/i.test(name);
const isMarkdownFile = (name: string) => /\.(?:md|markdown|mdown|mkd)$/i.test(name);

/** Map file extension → Prism grammar name */
const EXT_TO_LANG: Record<string, string> = {
    html: 'markup', htm: 'markup', xml: 'markup', svg: 'markup',
    js: 'javascript', mjs: 'javascript', cjs: 'javascript',
    ts: 'typescript', tsx: 'tsx', jsx: 'jsx',
    css: 'css', scss: 'scss',
    json: 'json', jsonc: 'json',
    md: 'markdown', markdown: 'markdown',
    py: 'python', pyw: 'python',
    rs: 'rust',
    go: 'go',
    java: 'java',
    c: 'c', h: 'c',
    cpp: 'cpp', cc: 'cpp', cxx: 'cpp', hpp: 'cpp',
    php: 'php',
    sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql',
    yaml: 'yaml', yml: 'yaml',
    toml: 'toml',
    dockerfile: 'docker',
};

function getLanguageFromFileName(name: string): string {
    const ext = name.split('.').pop()?.toLowerCase() || '';
    // Handle Dockerfile without extension
    if (name.toLowerCase() === 'dockerfile') return 'docker';
    return EXT_TO_LANG[ext] || '';
}


type ViewportPreset = 'mobile' | 'tablet' | 'desktop';
const VIEWPORT_WIDTHS: Record<ViewportPreset, number | null> = { mobile: 375, tablet: 768, desktop: null };
const ZOOM_LEVELS = [50, 75, 100, 125, 150];

/**
 * For HTML files: inline local <link rel="stylesheet"> as <style> blocks
 * so the iframe srcdoc renders with correct styles.
 */
async function inlineLocalStyles(html: string, filePath: string): Promise<string> {
    const dir = filePath.substring(0, filePath.lastIndexOf('/') + 1);
    const linkRegex = /<link\s+[^>]*rel=["']stylesheet["'][^>]*href=["']([^"']+)["'][^>]*\/?>/gi;
    let processed = html;
    const matches: { fullMatch: string; href: string }[] = [];

    let m: RegExpExecArray | null;
    while ((m = linkRegex.exec(html)) !== null) {
        matches.push({ fullMatch: m[0], href: m[1] });
    }

    for (const { fullMatch, href } of matches) {
        // Skip CDN / absolute URLs
        if (href.startsWith('http://') || href.startsWith('https://') || href.startsWith('//')) continue;
        const cssPath = href.startsWith('/') ? href : dir + href;
        try {
            const cssContent = await invoke<string>('read_local_file', { path: cssPath });
            processed = processed.replace(fullMatch, `<style>/* ${href} */\n${cssContent}</style>`);
        } catch {
            // CSS file not found — keep original link tag
        }
    }
    return processed;
}

export const TextViewer: React.FC<TextViewerProps> = ({
    file,
    onError,
    className = '',
}) => {
    const [content, setContent] = useState<string>('');
    const [isLoading, setIsLoading] = useState(true);
    const [copied, setCopied] = useState(false);
    const [wordWrap, setWordWrap] = useState(true);
    const [renderMode, setRenderMode] = useState(false);
    const [processedHtml, setProcessedHtml] = useState<string>('');

    // Dev tools state (HTML preview)
    const [viewport, setViewport] = useState<ViewportPreset>('desktop');
    const [zoom, setZoom] = useState(100);
    const [pickedColor, setPickedColor] = useState<string | null>(null);
    const [autoRefresh, setAutoRefresh] = useState(false);
    const iframeRef = useRef<HTMLIFrameElement>(null);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const hasEyeDropper = typeof (window as any).EyeDropper === 'function';

    const canRender = isHTMLFile(file.name) || isMarkdownFile(file.name);
    const isHTML = isHTMLFile(file.name);
    const isMD = isMarkdownFile(file.name);

    // Load text content
    useEffect(() => {
        const loadContent = async () => {
            setIsLoading(true);
            try {
                if (typeof file.content === 'string' && file.content.length > 0) {
                    setContent(file.content);
                } else if (file.blobUrl) {
                    const response = await fetch(file.blobUrl);
                    const text = await response.text();
                    setContent(text);
                } else {
                    setContent('No content available');
                }
            } catch (err) {
                console.error('Failed to load text content:', err);
                onError?.('Failed to load text content');
                setContent('Error loading content');
            } finally {
                setIsLoading(false);
            }
        };

        loadContent();
    }, [file, onError]);

    // Process HTML for iframe rendering (inline local CSS)
    useEffect(() => {
        if (!renderMode || !isHTML || !content) return;
        let cancelled = false;
        inlineLocalStyles(content, file.path).then(result => {
            if (!cancelled) setProcessedHtml(result);
        });
        return () => { cancelled = true; };
    }, [renderMode, isHTML, content, file.path]);

    // Auto-refresh: re-read file every 3s
    useEffect(() => {
        if (!autoRefresh || !renderMode || !isHTML) return;
        const id = setInterval(async () => {
            try {
                const fresh = await invoke<string>('read_local_file', { path: file.path });
                setContent(fresh);
            } catch { /* ignore */ }
        }, 3000);
        return () => clearInterval(id);
    }, [autoRefresh, renderMode, isHTML, file.path]);

    // Copy to clipboard
    const copyToClipboard = async () => {
        try {
            await navigator.clipboard.writeText(content);
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        } catch (err) {
            console.error('Failed to copy:', err);
        }
    };

    // Download file
    const downloadFile = () => {
        const blob = new Blob([content], { type: 'text/plain' });
        const url = URL.createObjectURL(blob);
        const link = document.createElement('a');
        link.href = url;
        link.download = file.name;
        link.click();
        URL.revokeObjectURL(url);
    };

    // Open in system browser
    const openInBrowser = async () => {
        try {
            const { open } = await import('@tauri-apps/plugin-shell');
            await open(`file://${file.path}`);
        } catch {
            // Fallback: write temp and open
        }
    };

    // EyeDropper color picker (Chromium-based webviews only)
    const pickColor = async () => {
        if (!hasEyeDropper) return;
        try {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            const dropper = new (window as any).EyeDropper();
            const result = await dropper.open();
            setPickedColor(result.sRGBHex);
            await navigator.clipboard.writeText(result.sRGBHex);
        } catch {
            // User cancelled
            setPickedColor(null);
        }
    };

    const toggleRender = useCallback(() => setRenderMode(prev => !prev), []);

    // Manual refresh
    const manualRefresh = async () => {
        try {
            const fresh = await invoke<string>('read_local_file', { path: file.path });
            setContent(fresh);
        } catch { /* ignore */ }
    };

    // Get line count
    const lineCount = content.split('\n').length;
    const charCount = content.length;

    // Viewport width for iframe container
    const vpWidth = VIEWPORT_WIDTHS[viewport];

    // Syntax highlighting via Prism.js
    const lang = useMemo(() => getLanguageFromFileName(file.name), [file.name]);
    const highlightedHtml = useMemo(() => {
        if (!content || !lang) return '';
        const grammar = Prism.languages[lang];
        if (!grammar) return '';
        return Prism.highlight(content, grammar, lang);
    }, [content, lang]);

    if (isLoading) {
        return (
            <div className={`flex items-center justify-center h-full bg-gray-900 ${className}`}>
                <div className="text-gray-400 animate-pulse">Loading...</div>
            </div>
        );
    }

    return (
        <div className={`flex flex-col h-full bg-gray-900 ${className}`}>
            {/* Primary Toolbar */}
            <div className="flex items-center justify-between px-4 py-2 bg-gray-800 border-b border-gray-700">
                <div className="flex items-center gap-2">
                    <FileText size={16} className="text-blue-400" />
                    <span className="text-sm font-medium text-gray-300 truncate max-w-xs">{file.name}</span>
                    <span className="text-xs text-gray-500">
                        {lineCount} lines • {charCount.toLocaleString()} chars
                    </span>
                </div>

                <div className="flex items-center gap-2">
                    {/* Render toggle — only for HTML/Markdown */}
                    {canRender && (
                        <button
                            onClick={toggleRender}
                            className={`p-1.5 rounded transition-colors flex items-center gap-1 text-xs ${renderMode ? 'bg-purple-500/20 text-purple-400' : 'hover:bg-gray-700 text-gray-400'}`}
                            title={renderMode ? 'Show Source Code' : 'Render Preview'}
                        >
                            {renderMode ? <Code2 size={16} /> : <Eye size={16} />}
                            <span className="hidden sm:inline">{renderMode ? 'Source' : 'Preview'}</span>
                        </button>
                    )}

                    {/* Word wrap toggle — hide in render mode */}
                    {!renderMode && (
                        <button
                            onClick={() => setWordWrap(!wordWrap)}
                            className={`p-1.5 rounded transition-colors ${wordWrap ? 'bg-blue-500/20 text-blue-400' : 'hover:bg-gray-700 text-gray-400'}`}
                            title={wordWrap ? 'Disable Word Wrap' : 'Enable Word Wrap'}
                        >
                            <WrapText size={16} />
                        </button>
                    )}

                    {/* Copy button */}
                    <button
                        onClick={copyToClipboard}
                        className="p-1.5 rounded hover:bg-gray-700 text-gray-400 hover:text-white transition-colors"
                        title="Copy to Clipboard"
                    >
                        {copied ? <Check size={16} className="text-green-400" /> : <Copy size={16} />}
                    </button>

                    {/* Download button */}
                    <button
                        onClick={downloadFile}
                        className="p-1.5 rounded hover:bg-gray-700 text-gray-400 hover:text-white transition-colors"
                        title="Download File"
                    >
                        <Download size={16} />
                    </button>
                </div>
            </div>

            {/* Dev Tools Bar — only in HTML render mode */}
            {renderMode && isHTML && (
                <div className="flex items-center gap-3 px-4 py-1.5 bg-gray-800/60 border-b border-gray-700/50">
                    {/* Viewport presets */}
                    <div className="flex items-center gap-1">
                        <button
                            onClick={() => setViewport('mobile')}
                            className={`p-1 rounded transition-colors ${viewport === 'mobile' ? 'bg-blue-500/20 text-blue-400' : 'text-gray-500 hover:text-gray-300'}`}
                            title="Mobile (375px)"
                        >
                            <Smartphone size={14} />
                        </button>
                        <button
                            onClick={() => setViewport('tablet')}
                            className={`p-1 rounded transition-colors ${viewport === 'tablet' ? 'bg-blue-500/20 text-blue-400' : 'text-gray-500 hover:text-gray-300'}`}
                            title="Tablet (768px)"
                        >
                            <Tablet size={14} />
                        </button>
                        <button
                            onClick={() => setViewport('desktop')}
                            className={`p-1 rounded transition-colors ${viewport === 'desktop' ? 'bg-blue-500/20 text-blue-400' : 'text-gray-500 hover:text-gray-300'}`}
                            title="Desktop (100%)"
                        >
                            <Monitor size={14} />
                        </button>
                        {vpWidth && (
                            <span className="text-[10px] text-gray-500 ml-1">{vpWidth}px</span>
                        )}
                    </div>

                    <div className="w-px h-4 bg-gray-700" />

                    {/* Zoom */}
                    <div className="flex items-center gap-1">
                        <button
                            onClick={() => setZoom(z => Math.max(50, z - 25))}
                            className="p-1 text-gray-500 hover:text-gray-300 rounded transition-colors"
                            title="Zoom Out"
                        >
                            <ZoomOut size={14} />
                        </button>
                        <select
                            value={zoom}
                            onChange={e => setZoom(Number(e.target.value))}
                            className="bg-transparent text-xs text-gray-400 border border-gray-700 rounded px-1 py-0.5 focus:outline-none"
                        >
                            {ZOOM_LEVELS.map(z => (
                                <option key={z} value={z} className="bg-gray-800">{z}%</option>
                            ))}
                        </select>
                        <button
                            onClick={() => setZoom(z => Math.min(150, z + 25))}
                            className="p-1 text-gray-500 hover:text-gray-300 rounded transition-colors"
                            title="Zoom In"
                        >
                            <ZoomIn size={14} />
                        </button>
                    </div>

                    {/* Color Picker — only shown when EyeDropper API is available */}
                    {hasEyeDropper && (
                        <>
                            <div className="w-px h-4 bg-gray-700" />
                            <button
                                onClick={pickColor}
                                className={`p-1 rounded transition-colors flex items-center gap-1 ${pickedColor ? 'text-purple-400' : 'text-gray-500 hover:text-gray-300'}`}
                                title="Pick Color (copies to clipboard)"
                            >
                                <Pipette size={14} />
                                {pickedColor && (
                                    <span className="text-[10px] flex items-center gap-1">
                                        <span className="w-3 h-3 rounded-sm border border-gray-600 inline-block" style={{ backgroundColor: pickedColor }} />
                                        {pickedColor}
                                    </span>
                                )}
                            </button>
                        </>
                    )}

                    <div className="w-px h-4 bg-gray-700" />

                    {/* Refresh */}
                    <button
                        onClick={manualRefresh}
                        className="p-1 text-gray-500 hover:text-gray-300 rounded transition-colors"
                        title="Refresh Preview"
                    >
                        <RefreshCw size={14} />
                    </button>

                    {/* Auto-refresh */}
                    <button
                        onClick={() => setAutoRefresh(a => !a)}
                        className={`p-1 rounded transition-colors text-[10px] px-1.5 ${autoRefresh ? 'bg-green-500/20 text-green-400' : 'text-gray-500 hover:text-gray-300'}`}
                        title={autoRefresh ? 'Stop Auto-Refresh' : 'Auto-Refresh (3s)'}
                    >
                        {autoRefresh ? 'Auto ●' : 'Auto'}
                    </button>

                    <div className="flex-1" />

                    {/* Open in Browser */}
                    <button
                        onClick={openInBrowser}
                        className="p-1 text-gray-500 hover:text-gray-300 rounded transition-colors flex items-center gap-1 text-[10px]"
                        title="Open in System Browser"
                    >
                        <ExternalLink size={14} />
                        <span className="hidden sm:inline">Browser</span>
                    </button>
                </div>
            )}

            {/* Content area */}
            {renderMode && isMD ? (
                /* Markdown rendered preview */
                <div className="flex-1 overflow-auto p-6 prose prose-invert max-w-none">
                    <MarkdownRenderer content={content} />
                </div>
            ) : renderMode && isHTML ? (
                /* HTML rendered preview with viewport + zoom */
                <div className="flex-1 overflow-auto flex justify-center bg-gray-950/50 p-4">
                    <div
                        className="transition-all duration-200"
                        style={{
                            width: vpWidth ? `${vpWidth}px` : '100%',
                            maxWidth: '100%',
                            height: '100%',
                            transform: `scale(${zoom / 100})`,
                            transformOrigin: 'top center',
                        }}
                    >
                        <iframe
                            ref={iframeRef}
                            srcDoc={processedHtml || content}
                            sandbox=""
                            className="w-full h-full bg-white border border-gray-700 rounded"
                            title="HTML Preview"
                        />
                    </div>
                </div>
            ) : (
                /* Source code view with line numbers + syntax highlighting */
                <div className="flex-1 overflow-auto font-mono text-sm">
                    {/* Language badge */}
                    {lang && (
                        <div className="sticky top-0 z-10 flex justify-end px-3 py-1 bg-gray-800/80 border-b border-gray-700/50">
                            <span className="text-[10px] font-mono text-gray-400 uppercase">{lang}</span>
                        </div>
                    )}
                    <div className="flex min-h-full">
                        {/* Line numbers */}
                        <div className="flex-shrink-0 select-none text-right pr-4 pl-4 py-4 text-gray-600 bg-gray-800/50 border-r border-gray-700">
                            {content.split('\n').map((_, i) => (
                                <div key={i} className="leading-6">{i + 1}</div>
                            ))}
                        </div>

                        {/* Content with syntax highlighting */}
                        <pre
                            className={`flex-1 p-4 leading-6 select-text cursor-text ${wordWrap ? 'whitespace-pre-wrap break-words' : 'whitespace-pre'}`}
                        >
                            {highlightedHtml ? (
                                <code
                                    className={`language-${lang}`}
                                    dangerouslySetInnerHTML={{ __html: highlightedHtml }}
                                />
                            ) : (
                                <code className="text-gray-200">{content}</code>
                            )}
                        </pre>
                    </div>
                </div>
            )}
        </div>
    );
};

export default TextViewer;
