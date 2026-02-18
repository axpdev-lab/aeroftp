import { useState, useCallback } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { readFile } from '@tauri-apps/plugin-fs';
import { invoke } from '@tauri-apps/api/core';
import { VisionImage, MAX_IMAGE_SIZE, MAX_IMAGES, SUPPORTED_IMAGE_TYPES, MAX_DIMENSION } from './aiChatTypes';

export function useAIChatImages() {
    const [attachedImages, setAttachedImages] = useState<VisionImage[]>([]);

    // Resize image to max dimension using canvas
    const resizeImage = useCallback((dataUrl: string, maxDim: number): Promise<string> => {
        return new Promise((resolve) => {
            const img = new Image();
            img.onerror = () => resolve(dataUrl); // Fallback to original on error
            img.onload = () => {
                if (img.width <= maxDim && img.height <= maxDim) {
                    resolve(dataUrl);
                    return;
                }
                const scale = Math.min(maxDim / img.width, maxDim / img.height);
                const canvas = document.createElement('canvas');
                canvas.width = Math.round(img.width * scale);
                canvas.height = Math.round(img.height * scale);
                const ctx = canvas.getContext('2d')!;
                ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
                resolve(canvas.toDataURL('image/jpeg', 0.85));
            };
            img.src = dataUrl;
        });
    }, []);

    // Add an image from a data URL
    const addImage = useCallback(async (dataUrl: string, mediaType: string) => {
        if (attachedImages.length >= MAX_IMAGES) return;

        // Resize if needed
        const resized = await resizeImage(dataUrl, MAX_DIMENSION);
        const base64 = resized.split(',')[1] || resized;
        const finalMediaType = resized.startsWith('data:image/jpeg') ? 'image/jpeg' : mediaType;

        // Check size
        const sizeBytes = Math.round(base64.length * 0.75);
        if (sizeBytes > MAX_IMAGE_SIZE) return;

        setAttachedImages(prev => [...prev, {
            data: base64,
            mediaType: finalMediaType,
            preview: resized.startsWith('data:') ? resized : `data:${finalMediaType};base64,${base64}`,
        }]);
    }, [attachedImages.length, resizeImage]);

    // Pick image from filesystem
    const handleImagePick = useCallback(async () => {
        if (attachedImages.length >= MAX_IMAGES) return;
        try {
            const selected = await open({
                multiple: true,
                filters: [{ name: 'Images', extensions: ['jpg', 'jpeg', 'png', 'gif', 'webp'] }],
            });
            if (!selected) return;
            const paths = Array.isArray(selected) ? selected : [selected];
            for (const filePath of paths.slice(0, MAX_IMAGES - attachedImages.length)) {
                const bytes = await readFile(filePath);
                const ext = filePath.split('.').pop()?.toLowerCase() || 'png';
                const mimeMap: Record<string, string> = { jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', gif: 'image/gif', webp: 'image/webp' };
                const mediaType = mimeMap[ext] || 'image/png';

                // Convert Uint8Array to base64 (chunked to avoid O(n^2) string growth)
                let binary = '';
                const chunkSize = 8192;
                for (let i = 0; i < bytes.length; i += chunkSize) {
                    const chunk = bytes.subarray(i, Math.min(i + chunkSize, bytes.length));
                    binary += String.fromCharCode(...chunk);
                }
                const base64 = btoa(binary);
                const dataUrl = `data:${mediaType};base64,${base64}`;
                await addImage(dataUrl, mediaType);
            }
        } catch { /* dialog cancelled */ }
    }, [attachedImages.length, addImage]);

    // Convert RGBA data from native clipboard to canvas data URL
    const rgbaToDataUrl = useCallback((width: number, height: number, rgbaBase64: string): Promise<string> => {
        return new Promise((resolve, reject) => {
            const binary = atob(rgbaBase64);
            const rgba = new Uint8ClampedArray(binary.length);
            for (let i = 0; i < binary.length; i++) rgba[i] = binary.charCodeAt(i);
            const canvas = document.createElement('canvas');
            canvas.width = width;
            canvas.height = height;
            const ctx = canvas.getContext('2d');
            if (!ctx) return reject('No canvas context');
            const imageData = new ImageData(rgba, width, height);
            ctx.putImageData(imageData, 0, 0);
            resolve(canvas.toDataURL('image/png'));
        });
    }, []);

    // Handle clipboard paste for images
    const handlePaste = useCallback((e: React.ClipboardEvent) => {
        const items = e.clipboardData?.items;
        if (items) {
            for (const item of Array.from(items)) {
                if (item.type.startsWith('image/') && SUPPORTED_IMAGE_TYPES.includes(item.type)) {
                    e.preventDefault();
                    const blob = item.getAsFile();
                    if (!blob) continue;
                    const reader = new FileReader();
                    reader.onload = () => {
                        const dataUrl = reader.result as string;
                        addImage(dataUrl, item.type);
                    };
                    reader.readAsDataURL(blob);
                    return; // Only handle first image
                }
            }
        }

        // Fallback: read image from native clipboard via arboard (WebKitGTK support)
        e.preventDefault();
        invoke<string | null>('clipboard_read_image').then(async (result) => {
            if (!result) return;
            const [wStr, hStr, ...rest] = result.split(':');
            const w = parseInt(wStr, 10);
            const h = parseInt(hStr, 10);
            const rgbaBase64 = rest.join(':');
            if (!w || !h || !rgbaBase64) return;
            const dataUrl = await rgbaToDataUrl(w, h, rgbaBase64);
            addImage(dataUrl, 'image/png');
        }).catch(() => { /* No image in clipboard */ });
    }, [addImage, rgbaToDataUrl]);

    const removeImage = useCallback((index: number) => {
        setAttachedImages(prev => prev.filter((_, j) => j !== index));
    }, []);

    const clearImages = useCallback(() => setAttachedImages([]), []);

    return { attachedImages, setAttachedImages, addImage, handleImagePick, handlePaste, removeImage, clearImages };
}
