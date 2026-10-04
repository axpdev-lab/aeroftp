// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroImage Editor Sidebar Panel
 *
 * Rendered to the right of the image when edit mode is active.
 * Operations are grouped into a LOSSLESS section and a LOSSY section
 * (#270, regrouped per Ehud's wish in discussion #347): the label is
 * written once as the section title instead of repeating a badge on
 * every control. The backend (image_edit.rs) decodes and re-encodes
 * pixels, so on a lossy source (JPEG/GIF) even the pixel-exact
 * operations are re-encoded on save: the first section is then marked
 * LOSSY as well, with the shared format note explaining why.
 */

import React, { useState, useCallback, useMemo } from 'react';
import {
    Crop,
    RotateCw,
    RotateCcw,
    FlipHorizontal2,
    FlipVertical2,
    Link,
    Unlink,
    ChevronRight,
    RotateCcw as ResetIcon,
    Save,
    X,
    Sun,
    Contrast as ContrastIcon,
    Palette,
    Droplets,
    Sparkles,
} from 'lucide-react';
import { useI18n } from '../../../i18n';
import {
    EditState,
    INITIAL_EDIT_STATE,
    ImageMetadata,
    PreviewFileData,
    formatLossKind,
} from '../types';

interface ImageEditorProps {
    file: PreviewFileData;
    metadata: ImageMetadata | null;
    editState: EditState;
    onEditStateChange: (state: EditState) => void;
    onCropModeToggle: (active: boolean) => void;
    cropMode: boolean;
    onSaveRequest: () => void;
}

// Rotation cycle helper
const ROTATION_CYCLE: readonly (0 | 90 | 180 | 270)[] = [0, 90, 180, 270];

const RESIZE_PRESETS = [50, 75, 150, 200] as const;

// Reusable slider row
interface SliderRowProps {
    label: string;
    icon: React.ReactNode;
    value: number;
    min: number;
    max: number;
    step: number;
    unit?: string;
    onChange: (v: number) => void;
    onReset: () => void;
}

const SliderRow: React.FC<SliderRowProps> = React.memo(
    ({ label, icon, value, min, max, step, unit, onChange, onReset }) => (
        <div className="px-3 py-1.5">
            <div className="flex items-center justify-between mb-1">
                <div className="flex items-center gap-1.5 text-xs text-[var(--color-text-primary)]">
                    {icon}
                    <span>{label}</span>
                </div>
                <div className="flex items-center gap-1">
                    <span className="text-xs text-[var(--color-text-tertiary)] tabular-nums w-10 text-right">
                        {value}
                        {unit ?? ''}
                    </span>
                    {value !== 0 && (
                        <button
                            onClick={onReset}
                            className="p-0.5 text-[var(--color-text-tertiary)] hover:text-[var(--color-text-primary)] rounded"
                            title="Reset"
                        >
                            <X size={12} />
                        </button>
                    )}
                </div>
            </div>
            <input
                type="range"
                min={min}
                max={max}
                step={step}
                value={value}
                onChange={(e) => onChange(Number(e.target.value))}
                className="w-full h-1.5 bg-[var(--color-bg-tertiary)] rounded-lg appearance-none cursor-pointer accent-[var(--color-accent)]"
            />
        </div>
    )
);
SliderRow.displayName = 'SliderRow';

// Collapsible section wrapper. `tone` colors the title once (green for the
// LOSSLESS section, amber for LOSSY) so the word does not need repeating on
// every control inside.
interface SectionProps {
    title: string;
    tone?: 'green' | 'amber';
    hint?: string;
    expanded: boolean;
    onToggle: () => void;
    children: React.ReactNode;
}

const Section: React.FC<SectionProps> = ({ title, tone, hint, expanded, onToggle, children }) => (
    <div className="border-t border-[var(--color-border)]">
        <button
            onClick={onToggle}
            title={hint}
            className={`flex items-center justify-between w-full px-3 py-2 text-xs font-semibold uppercase tracking-wider cursor-pointer hover:bg-[var(--color-bg-tertiary)] ${
                tone === 'green'
                    ? 'text-green-400'
                    : tone === 'amber'
                        ? 'text-amber-400'
                        : 'text-[var(--color-text-secondary)]'
            }`}
        >
            <span>{title}</span>
            <ChevronRight
                size={14}
                className={`transition-transform duration-150 ${expanded ? 'rotate-90' : ''}`}
            />
        </button>
        {expanded && <div className="pb-2">{children}</div>}
    </div>
);

// Toggle button
interface ToggleBtnProps {
    active: boolean;
    onClick: () => void;
    children: React.ReactNode;
    title?: string;
}

const ToggleBtn: React.FC<ToggleBtnProps> = ({ active, onClick, children, title }) => (
    <button
        onClick={onClick}
        title={title}
        className={`flex items-center gap-1.5 px-2.5 py-1.5 text-xs rounded border transition-colors ${
            active
                ? 'bg-blue-600/20 text-blue-400 border-blue-500/50'
                : 'bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] border-[var(--color-border)] hover:bg-[var(--color-surface-hover)]'
        }`}
    >
        {children}
    </button>
);

const ImageEditor: React.FC<ImageEditorProps> = ({
    file,
    metadata,
    editState,
    onEditStateChange,
    onCropModeToggle,
    cropMode,
    onSaveRequest,
}) => {
    const { t } = useI18n();

    // Section collapse state (all expanded by default)
    const [losslessOpen, setLosslessOpen] = useState(true);
    const [lossyOpen, setLossyOpen] = useState(true);

    // Aspect ratio lock
    const [lockAspect, setLockAspect] = useState(true);

    const aspectRatio = useMemo(() => {
        if (!metadata || metadata.height === 0) return 1;
        return metadata.width / metadata.height;
    }, [metadata]);

    // Pixel-exact operations (crop, right-angle rotate, flip, invert) only stay
    // lossless when the save target stores pixels exactly. AeroImage re-encodes
    // on save, so on a lossy source (JPEG/GIF) the whole pipeline is lossy and
    // the first section is marked LOSSY too (Ehud, discussion #347).
    const sourceFormat = useMemo(() => {
        if (metadata?.format) return metadata.format;
        const ext = file.name.split('.').pop();
        return ext ?? '';
    }, [metadata, file.name]);
    const sourceLossy = formatLossKind(sourceFormat) === 'lossy';

    // Patch helper: merges partial state
    const patch = useCallback(
        (partial: Partial<EditState>) => {
            onEditStateChange({ ...editState, ...partial });
        },
        [editState, onEditStateChange]
    );

    // ─── Geometry handlers ────────────────────────────────────────────

    const handleRotateCW = useCallback(() => {
        const idx = ROTATION_CYCLE.indexOf(editState.rotation);
        patch({ rotation: ROTATION_CYCLE[(idx + 1) % 4] });
    }, [editState.rotation, patch]);

    const handleRotateCCW = useCallback(() => {
        const idx = ROTATION_CYCLE.indexOf(editState.rotation);
        patch({ rotation: ROTATION_CYCLE[(idx + 3) % 4] });
    }, [editState.rotation, patch]);

    const handleRotate180 = useCallback(() => {
        const idx = ROTATION_CYCLE.indexOf(editState.rotation);
        patch({ rotation: ROTATION_CYCLE[(idx + 2) % 4] });
    }, [editState.rotation, patch]);

    const handleResizeWidth = useCallback(
        (w: number) => {
            if (w <= 0) return;
            const h = lockAspect ? Math.round(w / aspectRatio) : editState.resize?.height ?? metadata?.height ?? w;
            patch({ resize: { width: w, height: h } });
        },
        [lockAspect, aspectRatio, editState.resize, metadata, patch]
    );

    const handleResizeHeight = useCallback(
        (h: number) => {
            if (h <= 0) return;
            const w = lockAspect ? Math.round(h * aspectRatio) : editState.resize?.width ?? metadata?.width ?? h;
            patch({ resize: { width: w, height: h } });
        },
        [lockAspect, aspectRatio, editState.resize, metadata, patch]
    );

    const handleResizePreset = useCallback(
        (pct: number) => {
            if (!metadata) return;
            const w = Math.round(metadata.width * pct / 100);
            const h = Math.round(metadata.height * pct / 100);
            patch({ resize: { width: w, height: h } });
        },
        [metadata, patch]
    );

    const currentW = editState.resize?.width ?? metadata?.width ?? 0;
    const currentH = editState.resize?.height ?? metadata?.height ?? 0;

    // ─── Render ───────────────────────────────────────────────────────

    return (
        <div className="w-[280px] bg-[var(--color-bg-secondary)] border-l border-white/10 flex flex-col overflow-y-auto shrink-0 select-none">
            {/* Header */}
            <div className="flex items-center justify-between px-3 py-2.5 border-b border-[var(--color-border)]">
                <span className="text-sm font-semibold text-[var(--color-text-primary)]">AeroImage</span>
                <button
                    onClick={() => onEditStateChange(INITIAL_EDIT_STATE)}
                    className="p-1 text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)] rounded hover:bg-[var(--color-bg-tertiary)]"
                    title={t('preview.image.edit.resetAll') || 'Reset All'}
                >
                    <ResetIcon size={16} />
                </button>
            </div>

            {/* ─── Lossless operations (LOSSY when the source is JPEG/GIF) ── */}
            <Section
                title={
                    sourceLossy
                        ? t('preview.image.edit.lossy') || 'Lossy'
                        : t('preview.image.edit.lossless') || 'Lossless'
                }
                tone={sourceLossy ? 'amber' : 'green'}
                hint={
                    sourceLossy
                        ? t('preview.image.edit.lossyHint') || 'Alters pixels: the change cannot be perfectly undone'
                        : t('preview.image.edit.losslessHint') || 'Reversible: pixels are preserved exactly when saved to a lossless format'
                }
                expanded={losslessOpen}
                onToggle={() => setLosslessOpen((p) => !p)}
            >
                {sourceLossy && (
                    <p className="px-3 pt-1 text-[10px] text-amber-400/90 italic leading-snug">
                        {t('preview.image.edit.formatLossyNote') || 'Lossy format: re-encoding discards some image data, including any lossless edits.'}
                    </p>
                )}

                {/* Crop toggle */}
                <div className="px-3 py-1.5 flex items-center gap-2">
                    <ToggleBtn
                        active={cropMode}
                        onClick={() => onCropModeToggle(!cropMode)}
                        title={t('preview.image.edit.crop') || 'Crop'}
                    >
                        <Crop size={14} />
                        <span>{t('preview.image.edit.crop') || 'Crop'}</span>
                    </ToggleBtn>
                </div>

                {/* Rotate */}
                <div className="px-3 py-1.5">
                    <div className="flex items-center gap-1.5 mb-1.5">
                        <span className="text-xs text-[var(--color-text-secondary)]">
                            {t('preview.image.edit.rotate') || 'Rotate'}
                        </span>
                    </div>
                    <div className="flex gap-1.5">
                        <button
                            onClick={handleRotateCCW}
                            className="flex items-center gap-1 px-2 py-1.5 text-xs bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)] rounded border border-[var(--color-border)] hover:bg-[var(--color-surface-hover)]"
                            title="90° CCW"
                        >
                            <RotateCcw size={14} /> 90°
                        </button>
                        <button
                            onClick={handleRotate180}
                            className="flex items-center gap-1 px-2 py-1.5 text-xs bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)] rounded border border-[var(--color-border)] hover:bg-[var(--color-surface-hover)]"
                            title="180°"
                        >
                            180°
                        </button>
                        <button
                            onClick={handleRotateCW}
                            className="flex items-center gap-1 px-2 py-1.5 text-xs bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)] rounded border border-[var(--color-border)] hover:bg-[var(--color-surface-hover)]"
                            title="90° CW"
                        >
                            <RotateCw size={14} /> 90°
                        </button>
                    </div>
                </div>

                {/* Flip */}
                <div className="px-3 py-1.5">
                    <div className="flex items-center gap-1.5 mb-1.5">
                        <span className="text-xs text-[var(--color-text-secondary)]">
                            {t('preview.image.edit.flip') || 'Flip'}
                        </span>
                    </div>
                    <div className="flex gap-1.5">
                        <ToggleBtn
                            active={editState.flipH}
                            onClick={() => patch({ flipH: !editState.flipH })}
                            title="Flip Horizontal"
                        >
                            <FlipHorizontal2 size={14} />
                        </ToggleBtn>
                        <ToggleBtn
                            active={editState.flipV}
                            onClick={() => patch({ flipV: !editState.flipV })}
                            title="Flip Vertical"
                        >
                            <FlipVertical2 size={14} />
                        </ToggleBtn>
                    </div>
                </div>

                {/* Invert */}
                <div className="px-3 py-1.5">
                    <ToggleBtn
                        active={editState.invert}
                        onClick={() => patch({ invert: !editState.invert })}
                    >
                        {t('preview.image.edit.invert') || 'Invert'}
                    </ToggleBtn>
                </div>
            </Section>

            {/* ─── Lossy operations ────────────────────────────────────── */}
            <Section
                title={t('preview.image.edit.lossy') || 'Lossy'}
                tone="amber"
                hint={t('preview.image.edit.lossyHint') || 'Alters pixels: the change cannot be perfectly undone'}
                expanded={lossyOpen}
                onToggle={() => setLossyOpen((p) => !p)}
            >
                {/* Resize */}
                <div className="px-3 py-1.5">
                    <div className="flex items-center justify-between mb-1.5">
                        <span className="text-xs text-[var(--color-text-secondary)]">
                            {t('preview.image.edit.resize') || 'Resize'}
                        </span>
                        <button
                            onClick={() => setLockAspect((p) => !p)}
                            className={`p-1 rounded ${
                                lockAspect
                                    ? 'text-blue-400 hover:text-blue-300'
                                    : 'text-[var(--color-text-tertiary)] hover:text-[var(--color-text-primary)]'
                            }`}
                            title={
                                lockAspect
                                    ? t('preview.image.edit.unlockAspect') || 'Unlock aspect ratio'
                                    : t('preview.image.edit.lockAspect') || 'Lock aspect ratio'
                            }
                        >
                            {lockAspect ? <Link size={14} /> : <Unlink size={14} />}
                        </button>
                    </div>
                    <div className="flex items-center gap-2 mb-2">
                        <label className="text-xs text-[var(--color-text-tertiary)]">W</label>
                        <input
                            type="number"
                            min={1}
                            value={currentW}
                            onChange={(e) => handleResizeWidth(Number(e.target.value))}
                            className="w-20 bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] rounded px-2 py-1 text-sm text-[var(--color-text-primary)]"
                        />
                        <label className="text-xs text-[var(--color-text-tertiary)]">H</label>
                        <input
                            type="number"
                            min={1}
                            value={currentH}
                            onChange={(e) => handleResizeHeight(Number(e.target.value))}
                            className="w-20 bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] rounded px-2 py-1 text-sm text-[var(--color-text-primary)]"
                        />
                    </div>
                    <div className="flex gap-1.5 flex-wrap">
                        {RESIZE_PRESETS.map((pct) => (
                            <button
                                key={pct}
                                onClick={() => handleResizePreset(pct)}
                                className="px-2 py-1 text-xs bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] rounded border border-[var(--color-border)] hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text-primary)]"
                            >
                                {pct}%
                            </button>
                        ))}
                    </div>
                </div>

                <SliderRow
                    label={t('preview.image.edit.brightness') || 'Brightness'}
                    icon={<Sun size={13} />}
                    value={editState.brightness}
                    min={-100}
                    max={100}
                    step={1}
                    onChange={(v) => patch({ brightness: v })}
                    onReset={() => patch({ brightness: 0 })}
                />
                <SliderRow
                    label={t('preview.image.edit.contrast') || 'Contrast'}
                    icon={<ContrastIcon size={13} />}
                    value={editState.contrast}
                    min={-100}
                    max={100}
                    step={1}
                    onChange={(v) => patch({ contrast: v })}
                    onReset={() => patch({ contrast: 0 })}
                />
                <SliderRow
                    label={t('preview.image.edit.hue') || 'Hue'}
                    icon={<Palette size={13} />}
                    value={editState.hue}
                    min={-180}
                    max={180}
                    step={1}
                    unit="°"
                    onChange={(v) => patch({ hue: v })}
                    onReset={() => patch({ hue: 0 })}
                />
                <SliderRow
                    label={t('preview.image.edit.blur') || 'Blur'}
                    icon={<Droplets size={13} />}
                    value={editState.blur}
                    min={0}
                    max={10}
                    step={0.1}
                    onChange={(v) => patch({ blur: v })}
                    onReset={() => patch({ blur: 0 })}
                />
                <SliderRow
                    label={t('preview.image.edit.sharpen') || 'Sharpen'}
                    icon={<Sparkles size={13} />}
                    value={editState.sharpen}
                    min={0}
                    max={10}
                    step={0.1}
                    onChange={(v) => patch({ sharpen: v })}
                    onReset={() => patch({ sharpen: 0 })}
                />
                {editState.sharpen > 0 && (
                    <div className="px-3 text-[10px] text-[var(--color-text-tertiary)] italic">
                        {t('preview.image.edit.sharpenNote') || 'Applied on save'}
                    </div>
                )}
                <div className="px-3 py-1.5">
                    <ToggleBtn
                        active={editState.grayscale}
                        onClick={() => patch({ grayscale: !editState.grayscale })}
                    >
                        {t('preview.image.edit.grayscale') || 'Grayscale'}
                    </ToggleBtn>
                </div>
            </Section>

            {/* ─── Save Button ───────────────────────────────────────── */}
            <div className="mt-auto border-t border-[var(--color-border)] p-3">
                <button
                    onClick={onSaveRequest}
                    className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-green-600 hover:bg-green-500 text-white text-sm font-medium rounded transition-colors"
                >
                    <Save size={16} />
                    {t('preview.image.edit.saveTitle') || 'Save Image'}
                </button>
            </div>
        </div>
    );
};

export default ImageEditor;
