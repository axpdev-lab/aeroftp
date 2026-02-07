/**
 * Audio Mixer / Equalizer Component
 *
 * 10-band graphic equalizer with presets and balance control.
 * Uses Web Audio API BiquadFilterNode for each frequency band.
 *
 * Bands: 32Hz, 64Hz, 125Hz, 250Hz, 500Hz, 1kHz, 2kHz, 4kHz, 8kHz, 16kHz
 */

import React, { useState, useCallback, useEffect, useRef } from 'react';
import { Sliders, RotateCcw, ChevronDown, Volume2 } from 'lucide-react';
import { EqualizerState, EQPreset } from '../types';

// EQ frequency bands
export const EQ_BANDS = [
    { freq: 32, label: '32' },
    { freq: 64, label: '64' },
    { freq: 125, label: '125' },
    { freq: 250, label: '250' },
    { freq: 500, label: '500' },
    { freq: 1000, label: '1k' },
    { freq: 2000, label: '2k' },
    { freq: 4000, label: '4k' },
    { freq: 8000, label: '8k' },
    { freq: 16000, label: '16k' },
];

// EQ Presets
export const EQ_PRESETS: EQPreset[] = [
    { name: 'Flat', bands: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
    { name: 'Rock', bands: [4, 3, 0, -2, -3, 0, 2, 3, 4, 4] },
    { name: 'Pop', bands: [-1, 1, 3, 4, 3, 0, -1, -1, 1, 2] },
    { name: 'Jazz', bands: [3, 2, 0, 2, -2, -2, 0, 2, 3, 4] },
    { name: 'Classical', bands: [4, 3, 2, 1, -1, -1, 0, 2, 3, 4] },
    { name: 'Bass Boost', bands: [6, 5, 4, 2, 0, 0, 0, 0, 0, 0] },
    { name: 'Treble Boost', bands: [0, 0, 0, 0, 0, 1, 2, 4, 5, 6] },
    { name: 'Vocal', bands: [-2, -1, 0, 2, 4, 4, 3, 1, 0, -1] },
    { name: 'Electronic', bands: [4, 3, 0, -2, -1, 1, 0, 2, 4, 5] },
    { name: 'Acoustic', bands: [3, 2, 0, 1, 2, 1, 2, 2, 3, 2] },
];

interface AudioMixerProps {
    state: EqualizerState;
    onStateChange: (state: EqualizerState) => void;
    eqNodes?: BiquadFilterNode[];
    pannerNode?: StereoPannerNode | null;
    isExpanded?: boolean;
    onToggleExpand?: () => void;
    className?: string;
}

export const AudioMixer: React.FC<AudioMixerProps> = ({
    state,
    onStateChange,
    eqNodes,
    pannerNode,
    isExpanded = false,
    onToggleExpand,
    className = '',
}) => {
    const [showPresetMenu, setShowPresetMenu] = useState(false);
    // Store band values for restore when toggling enabled
    const savedBandsRef = useRef<number[]>([...state.bands]);

    // Sync Web Audio nodes when they arrive or when state.enabled changes
    useEffect(() => {
        if (!eqNodes || eqNodes.length === 0) return;
        for (let i = 0; i < eqNodes.length && i < state.bands.length; i++) {
            eqNodes[i].gain.value = state.enabled ? state.bands[i] : 0;
        }
    }, [eqNodes, state.enabled]);

    // Sync panner node when it arrives
    useEffect(() => {
        if (pannerNode) {
            pannerNode.pan.value = state.enabled ? state.balance : 0;
        }
    }, [pannerNode, state.balance, state.enabled]);

    // Handle band change — update both state and real Web Audio node
    const handleBandChange = useCallback((index: number, value: number) => {
        // Apply to real node immediately for zero-latency feedback
        if (eqNodes && eqNodes[index] && state.enabled) {
            eqNodes[index].gain.value = value;
        }
        const newBands = [...state.bands];
        newBands[index] = value;
        savedBandsRef.current = newBands;
        onStateChange({
            ...state,
            bands: newBands,
            presetName: 'Custom',
        });
    }, [state, onStateChange, eqNodes]);

    // Handle balance change — update both state and real StereoPannerNode
    const handleBalanceChange = useCallback((value: number) => {
        if (pannerNode && state.enabled) {
            pannerNode.pan.value = value;
        }
        onStateChange({
            ...state,
            balance: value,
        });
    }, [state, onStateChange, pannerNode]);

    // Apply preset — update all nodes + state
    const applyPreset = useCallback((preset: EQPreset) => {
        if (eqNodes && state.enabled) {
            for (let i = 0; i < eqNodes.length && i < preset.bands.length; i++) {
                eqNodes[i].gain.value = preset.bands[i];
            }
        }
        savedBandsRef.current = [...preset.bands];
        onStateChange({
            ...state,
            bands: [...preset.bands],
            presetName: preset.name,
        });
        setShowPresetMenu(false);
    }, [state, onStateChange, eqNodes]);

    // Reset to flat
    const resetEQ = useCallback(() => {
        const flatPreset = EQ_PRESETS[0];
        applyPreset(flatPreset);
    }, [applyPreset]);

    // Toggle EQ enabled — bypass (gain=0) or restore saved values
    const toggleEnabled = useCallback(() => {
        const willBeEnabled = !state.enabled;
        if (eqNodes) {
            for (let i = 0; i < eqNodes.length; i++) {
                eqNodes[i].gain.value = willBeEnabled ? (savedBandsRef.current[i] ?? 0) : 0;
            }
        }
        onStateChange({
            ...state,
            enabled: willBeEnabled,
        });
    }, [state, onStateChange, eqNodes]);

    if (!isExpanded) {
        return (
            <button
                onClick={onToggleExpand}
                className={`flex items-center gap-2 px-3 py-2 bg-gray-800 hover:bg-gray-700 border border-gray-700 rounded-lg transition-colors ${className}`}
            >
                <Sliders size={16} className="text-purple-400" />
                <span className="text-sm text-gray-300">Mixer</span>
                <ChevronDown size={14} className="text-gray-500" />
            </button>
        );
    }

    return (
        <div className={`bg-gray-800 border border-gray-700 rounded-xl p-4 ${className}`}>
            {/* Header */}
            <div className="flex items-center justify-between mb-4">
                <div className="flex items-center gap-2">
                    <Sliders size={18} className="text-purple-400" />
                    <span className="text-sm font-medium text-white">AeroMixer</span>
                    <span className="text-xs text-gray-500 font-mono">// EQ</span>
                </div>

                <div className="flex items-center gap-2">
                    {/* Preset selector */}
                    <div className="relative">
                        <button
                            onClick={() => setShowPresetMenu(!showPresetMenu)}
                            className="flex items-center gap-1 px-2 py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs text-gray-300 transition-colors"
                        >
                            {state.presetName}
                            <ChevronDown size={12} />
                        </button>

                        {showPresetMenu && (
                            <div className="absolute top-full right-0 mt-1 w-36 bg-gray-900 border border-gray-700 rounded-lg shadow-xl z-10 py-1">
                                {EQ_PRESETS.map((preset) => (
                                    <button
                                        key={preset.name}
                                        onClick={() => applyPreset(preset)}
                                        className={`w-full px-3 py-1.5 text-left text-xs hover:bg-gray-700 transition-colors ${state.presetName === preset.name ? 'text-blue-400' : 'text-gray-300'
                                            }`}
                                    >
                                        {preset.name}
                                    </button>
                                ))}
                            </div>
                        )}
                    </div>

                    {/* Reset button */}
                    <button
                        onClick={resetEQ}
                        className="p-1.5 hover:bg-gray-700 rounded transition-colors"
                        title="Reset to Flat"
                    >
                        <RotateCcw size={14} className="text-gray-400" />
                    </button>

                    {/* Enable/Disable toggle */}
                    <button
                        onClick={toggleEnabled}
                        className={`px-2 py-1 rounded text-xs font-medium transition-colors ${state.enabled
                                ? 'bg-purple-600 text-white'
                                : 'bg-gray-700 text-gray-400'
                            }`}
                    >
                        {state.enabled ? 'ON' : 'OFF'}
                    </button>
                </div>
            </div>

            {/* EQ Bands */}
            <div className="flex gap-1 justify-between mb-4">
                {EQ_BANDS.map((band, index) => (
                    <div key={band.freq} className="flex flex-col items-center">
                        {/* Slider (vertical) */}
                        <div className="relative h-24 w-4 flex items-center justify-center">
                            <input
                                type="range"
                                min="-12"
                                max="12"
                                step="1"
                                value={state.bands[index]}
                                onChange={(e) => handleBandChange(index, parseInt(e.target.value))}
                                disabled={!state.enabled}
                                className="absolute w-24 h-4 origin-center -rotate-90 appearance-none bg-transparent cursor-pointer disabled:opacity-50"
                                style={{
                                    background: 'transparent',
                                }}
                            />
                            {/* Visual track behind slider */}
                            <div className="absolute w-1.5 h-full bg-gray-700 rounded-full">
                                <div
                                    className={`absolute w-full rounded-full transition-all ${state.enabled
                                            ? 'bg-gradient-to-t from-blue-500 to-purple-500'
                                            : 'bg-gray-600'
                                        }`}
                                    style={{
                                        height: `${((state.bands[index] + 12) / 24) * 100}%`,
                                        bottom: 0,
                                    }}
                                />
                            </div>
                        </div>

                        {/* Value and label */}
                        <span className="text-[10px] text-gray-400 font-mono mt-1">
                            {state.bands[index] > 0 ? '+' : ''}{state.bands[index]}
                        </span>
                        <span className="text-[9px] text-gray-500">
                            {band.label}
                        </span>
                    </div>
                ))}
            </div>

            {/* Balance control */}
            <div className="flex items-center gap-3 pt-3 border-t border-gray-700">
                <Volume2 size={14} className="text-gray-500" />
                <span className="text-xs text-gray-500 w-6">L</span>
                <input
                    type="range"
                    min="-1"
                    max="1"
                    step="0.1"
                    value={state.balance}
                    onChange={(e) => handleBalanceChange(parseFloat(e.target.value))}
                    disabled={!state.enabled}
                    className="flex-1 h-1 appearance-none bg-gray-700 rounded-full cursor-pointer disabled:opacity-50"
                />
                <span className="text-xs text-gray-500 w-6 text-right">R</span>
                <span className="text-xs text-gray-400 font-mono w-8 text-right">
                    {state.balance === 0 ? 'C' : state.balance < 0 ? `L${Math.abs(state.balance * 100).toFixed(0)}` : `R${(state.balance * 100).toFixed(0)}`}
                </span>
            </div>

            {/* Close button */}
            {onToggleExpand && (
                <button
                    onClick={onToggleExpand}
                    className="w-full mt-3 py-1.5 text-xs text-gray-500 hover:text-gray-300 transition-colors"
                >
                    Hide Mixer
                </button>
            )}
        </div>
    );
};

export default AudioMixer;
