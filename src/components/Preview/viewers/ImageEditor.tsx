// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroImage Editor Sidebar Panel
 *
 * Rendered to the right of the image when edit mode is active.
 * Provides geometry transforms, color adjustments, and effects.
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
    LossKind,
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

// Lossless / lossy badge (#270). Each operation is tagged so the user knows
// whether it preserves the picture exactly (green) or alters pixels
// irreversibly (amber). The reading still depends on the chosen output format,
// which the Save dialog spells out.
const LossTag: React.FC<{ kind: LossKind }> = ({ kind }) => {
    const { t } = useI18n();
    const lossless = kind === 'lossless';
    return (
        <span
            title={
                lossless
                    ? t('preview.image.edit.losslessHint') || 'Reversible: pixels are preserved exactly when saved to a lossless format'
                    : t('preview.image.edit.lossyHint') || 'Alters pixels: the change cannot be perfectly undone'
            }
            className={`px-1.5 py-px rounded text-[9px] font-semibold uppercase tracking-wide border ${
                lossless
                    ? 'bg-green-500/15 text-green-400 border-green-500/30'
                    : 'bg-amber-500/15 text-amber-400 border-amber-500/30'
            }`}
        >
            {lossless
                ? t('preview.image.edit.lossless') || 'Lossless'
                : t('preview.image.edit.lossy') || 'Lossy'}
        </span>
    );
};

// Reusable slider row
interface SliderRowProps {
    label: string;
    icon: React.ReactNode;
    value: number;
    min: number;
    max: number;
    step: number;
    unit?: string;
    badge?: React.ReactNode;
    onChange: (v: number) => void;
    onReset: () => void;
}

const SliderRow: React.FC<SliderRowProps> = React.memo(
    ({ label, icon, value, min, max, step, unit, badge, onChange, onReset }) => (
        <div className="px-3 py-1.5">
            <div className="flex items-center justify-between mb-1">
                <div className="flex items-center gap-1.5 text-xs text-[var(--color-text-primary)]">
                    {icon}
                    <span>{label}</span>
                    {badge}
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

// Collapsible section wrapper
interface SectionProps {
    title: string;
    expanded: boolean;
    onToggle: () => void;
    children: React.ReactNode;
}

const Section: React.FC<SectionProps> = ({ title, expanded, onToggle, children }) => (
    <div className="border-t border-[var(--color-border)]">
        <button
            onClick={onToggle}
            className="flex items-center justify-between w-full px-3 py-2 text-xs font-semibold text-[var(--color-text-secondary)] uppercase tracking-wider cursor-pointer hover:bg-[var(--color-bg-tertiary)]"
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
    metadata,
    editState,
    onEditStateChange,
    onCropModeToggle,
    cropMode,
    onSaveRequest,
}) => {
    const { t } = useI18n();

    // Section collapse state (all expanded by default)
    const [geoOpen, setGeoOpen] = useState(true);
    const [adjOpen, setAdjOpen] = useState(true);
    const [fxOpen, setFxOpen] = useState(true);

    // Aspect ratio lock
    const [lockAspect, setLockAspect] = useState(true);

    const aspectRatio = useMemo(() => {
        if (!metadata || metadata.height === 0) return 1;
        return metadata.width / metadata.height;
    }, [metadata]);

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

            {/* ─── Geometry ──────────────────────────────────────────── */}
            <Section
                title={t('preview.image.edit.geometry') || 'Geometry'}
                expanded={geoOpen}
                onToggle={() => setGeoOpen((p) => !p)}
            >
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
                    <LossTag kind="lossless" />
                </div>

                {/* Rotate */}
                <div className="px-3 py-1.5">
                    <div className="flex items-center gap-1.5 mb-1.5">
                        <span className="text-xs text-[var(--color-text-secondary)]">
                            {t('preview.image.edit.rotate') || 'Rotate'}
                        </span>
                        <LossTag kind="lossless" />
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
                        <LossTag kind="lossless" />
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

                {/* Resize */}
                <div className="px-3 py-1.5">
                    <div className="flex items-center justify-between mb-1.5">
                        <div className="flex items-center gap-1.5">
                            <span className="text-xs text-[var(--color-text-secondary)]">
                                {t('preview.image.edit.resize') || 'Resize'}
                            </span>
                            <LossTag kind="lossy" />
                        </div>
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
            </Section>

            {/* ─── Adjustments ───────────────────────────────────────── */}
            <Section
                title={t('preview.image.edit.adjustments') || 'Adjustments'}
                expanded={adjOpen}
                onToggle={() => setAdjOpen((p) => !p)}
            >
                <SliderRow
                    label={t('preview.image.edit.brightness') || 'Brightness'}
                    icon={<Sun size={13} />}
                    value={editState.brightness}
                    min={-100}
                    max={100}
                    step={1}
                    badge={<LossTag kind="lossy" />}
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
                    badge={<LossTag kind="lossy" />}
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
                    badge={<LossTag kind="lossy" />}
                    onChange={(v) => patch({ hue: v })}
                    onReset={() => patch({ hue: 0 })}
                />
            </Section>

            {/* ─── Effects ───────────────────────────────────────────── */}
            <Section
                title={t('preview.image.edit.effects') || 'Effects'}
                expanded={fxOpen}
                onToggle={() => setFxOpen((p) => !p)}
            >
                <SliderRow
                    label={t('preview.image.edit.blur') || 'Blur'}
                    icon={<Droplets size={13} />}
                    value={editState.blur}
                    min={0}
                    max={10}
                    step={0.1}
                    badge={<LossTag kind="lossy" />}
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
                    badge={<LossTag kind="lossy" />}
                    onChange={(v) => patch({ sharpen: v })}
                    onReset={() => patch({ sharpen: 0 })}
                />
                {editState.sharpen > 0 && (
                    <div className="px-3 text-[10px] text-[var(--color-text-tertiary)] italic">
                        {t('preview.image.edit.sharpenNote') || 'Applied on save'}
                    </div>
                )}
                <div className="px-3 py-1.5 space-y-1.5">
                    <div className="flex items-center gap-2">
                        <ToggleBtn
                            active={editState.grayscale}
                            onClick={() => patch({ grayscale: !editState.grayscale })}
                        >
                            {t('preview.image.edit.grayscale') || 'Grayscale'}
                        </ToggleBtn>
                        <LossTag kind="lossy" />
                    </div>
                    <div className="flex items-center gap-2">
                        <ToggleBtn
                            active={editState.invert}
                            onClick={() => patch({ invert: !editState.invert })}
                        >
                            {t('preview.image.edit.invert') || 'Invert'}
                        </ToggleBtn>
                        <LossTag kind="lossless" />
                    </div>
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
