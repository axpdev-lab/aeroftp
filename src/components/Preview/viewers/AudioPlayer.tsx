/**
 * Audio Player Component - AeroPlayer CYBER EDITION 🔥
 * 
 * Full-featured audio player powered by Howler.js:
 * - Robust streaming and buffering for large files
 * - Playback controls (play/pause/seek/volume)
 * - Real-time visualizer with multiple modes
 * - 10-band graphic equalizer
 * - Keyboard shortcuts
 * - Playback speed control & Loop toggle
 */

import React, { useRef, useState, useEffect, useCallback } from 'react';
import { Howl, Howler } from 'howler';
import {
    Play, Pause, SkipBack, SkipForward, Volume2, VolumeX,
    Repeat, Gauge, Activity, BarChart2, Circle, Waves, Zap, ChevronDown, Loader2,
    Sparkles, Flame, Maximize2, Minimize2, Eye, RefreshCw, AlertTriangle
} from 'lucide-react';
import { ViewerBaseProps, PlaybackState, EqualizerState, MediaMetadata } from '../types';
import { formatDuration } from '../utils/fileTypes';
import { AudioVisualizer, VisualizerMode } from '../controls/AudioVisualizer';
import { AudioMixer, EQ_BANDS } from '../controls/AudioMixer';
import { logger } from '../../../utils/logger';

interface AudioPlayerProps extends ViewerBaseProps {
    className?: string;
}

// Default EQ state
const defaultEQState: EqualizerState = {
    enabled: true,
    bands: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    balance: 0,
    presetName: 'Flat',
};

// Visualizer mode options
const VISUALIZER_MODES: { value: VisualizerMode; label: string; icon: React.ReactNode }[] = [
    { value: 'bars', label: 'Bars', icon: <BarChart2 size={14} /> },
    { value: 'waveform', label: 'Waveform', icon: <Activity size={14} /> },
    { value: 'radial', label: 'Radial', icon: <Circle size={14} /> },
    { value: 'spectrum', label: 'Spectrum', icon: <Waves size={14} /> },
    { value: 'fractal', label: 'Fractal', icon: <Sparkles size={14} /> },
    { value: 'vortex', label: 'Vortex', icon: <Zap size={14} /> },
    { value: 'plasma', label: 'Plasma', icon: <Flame size={14} /> },
    { value: 'kaleidoscope', label: 'Kaleidoscope', icon: <Eye size={14} /> },
];

// Global map to track audio elements already connected to MediaElementSource
// This is necessary because once connected, an audio element cannot be reconnected
const connectedAudioElements = new WeakMap<HTMLMediaElement, {
    source: MediaElementAudioSourceNode;
    analyser: AnalyserNode;
}>();

export const AudioPlayer: React.FC<AudioPlayerProps> = ({
    file,
    onError,
    className = '',
}) => {
    // Refs
    const howlRef = useRef<Howl | null>(null);
    const analyserRef = useRef<AnalyserNode | null>(null);
    const sourceNodeRef = useRef<MediaElementAudioSourceNode | null>(null);
    const connectedElementRef = useRef<HTMLAudioElement | null>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const updateIntervalRef = useRef<number | null>(null);

    // State
    const [playback, setPlayback] = useState<PlaybackState>({
        isPlaying: false,
        currentTime: 0,
        duration: 0,
        volume: 0.8,
        isMuted: false,
        playbackRate: 1,
        isLooping: false,
        bufferedPercent: 0,
    });
    const [eqState, setEQState] = useState<EqualizerState>(defaultEQState);
    const [metadata, setMetadata] = useState<MediaMetadata | null>(null);
    const [visualizerMode, setVisualizerMode] = useState<VisualizerMode>('bars');
    const [cyberMode, setCyberMode] = useState(false);
    const [showMixer, setShowMixer] = useState(false);
    const [showVisualizerMenu, setShowVisualizerMenu] = useState(false);
    const [isAudioReady, setIsAudioReady] = useState(false);
    const [isBuffering, setIsBuffering] = useState(true);
    const [isFullscreen, setIsFullscreen] = useState(false);
    const [loadError, setLoadError] = useState<string | null>(null);
    const [retryCount, setRetryCount] = useState(0);
    const maxRetries = 3;

    // Audio source URL
    const audioSrc = file.blobUrl || file.content as string || '';

    // Extract format from filename for Howler (blob URLs don't have extensions)
    const audioFormat = file.name.split('.').pop()?.toLowerCase() || 'mp3';

    // Initialize Howler
    useEffect(() => {
        if (!audioSrc) return;

        // Cleanup previous instance
        if (howlRef.current) {
            howlRef.current.unload();
        }

        // Create new Howl instance
        // Large files (>10MB) use html5:true for streaming (no full decode needed)
        // Small files use html5:false for better Web Audio API integration
        const fileSizeBytes = file.size || 0;
        const isLargeFile = fileSizeBytes > 10_000_000; // 10MB threshold

        logger.debug(`Audio: ${file.name}, Size: ${(fileSizeBytes / 1024 / 1024).toFixed(1)}MB, Mode: ${isLargeFile ? 'HTML5 Streaming' : 'Web Audio API'}`);

        const howl = new Howl({
            src: [audioSrc],
            format: [audioFormat], // Specify format since blob URLs don't have extensions
            html5: isLargeFile, // Large files use streaming, small files use Web Audio decoding
            preload: true,
            volume: playback.volume,
            loop: playback.isLooping,
            onload: () => {
                setIsAudioReady(true);
                setIsBuffering(false);
                setLoadError(null);
                setRetryCount(0);
                setPlayback(prev => ({
                    ...prev,
                    duration: howl.duration(),
                }));
                setMetadata({
                    title: file.name.replace(/\\.[^/.]+$/, ''),
                });

                // Setup Web Audio API analyser for visualizer
                try {
                    const ctx = Howler.ctx;
                    if (ctx) {
                        if (isLargeFile) {
                            // HTML5 mode: Connect via MediaElementSource
                            // @ts-ignore - accessing internal Howler structure
                            const audioElement = howl._sounds?.[0]?._node as HTMLMediaElement | undefined;

                            if (audioElement && audioElement instanceof HTMLMediaElement) {
                                // Check if this element was already connected (from a previous load)
                                const existingConnection = connectedAudioElements.get(audioElement);

                                if (existingConnection) {
                                    // Reuse existing analyser from previous connection
                                    analyserRef.current = existingConnection.analyser;
                                    sourceNodeRef.current = existingConnection.source;
                                    logger.debug('Reusing existing MediaElementSource connection for visualizer');
                                } else {
                                    // Create new connection
                                    const analyser = ctx.createAnalyser();
                                    analyser.fftSize = 256;
                                    analyser.smoothingTimeConstant = 0.8;

                                    try {
                                        const source = ctx.createMediaElementSource(audioElement);
                                        source.connect(analyser);
                                        analyser.connect(ctx.destination);

                                        // Store in global map for future reuse
                                        connectedAudioElements.set(audioElement, { source, analyser });

                                        analyserRef.current = analyser;
                                        sourceNodeRef.current = source;
                                        connectedElementRef.current = audioElement;
                                        logger.debug('Visualizer connected via MediaElementSource (HTML5 streaming mode)');
                                    } catch (sourceErr: any) {
                                        if (sourceErr.name === 'InvalidStateError') {
                                            // Element was connected outside our tracking, no visualizer possible
                                            console.warn('Audio element already connected externally, visualizer disabled');
                                            analyserRef.current = analyser; // Still set analyser for UI, but it won't have data
                                        } else {
                                            throw sourceErr;
                                        }
                                    }
                                }
                            } else {
                                console.warn('No audio element found for HTML5 mode visualizer');
                            }
                        } else {
                            // Web Audio mode: Connect via masterGain (works because audio flows through it)
                            if (!analyserRef.current) {
                                const analyser = ctx.createAnalyser();
                                analyser.fftSize = 256;
                                analyser.smoothingTimeConstant = 0.8;
                                analyserRef.current = analyser;

                                Howler.masterGain.connect(analyser);
                                analyser.connect(ctx.destination);
                                logger.debug('Visualizer connected via masterGain (Web Audio mode)');
                            }
                        }
                    }
                } catch (err) {
                    console.warn('Failed to setup analyser:', err);
                }
            },
            onloaderror: (_id, error) => {
                console.error('Howler load error:', error);

                // Auto-retry logic for intermittent failures
                if (retryCount < maxRetries) {
                    logger.debug(`Retrying audio load (${retryCount + 1}/${maxRetries})...`);
                    setRetryCount(prev => prev + 1);
                    // Delay retry slightly to allow blob URL to stabilize
                    setTimeout(() => {
                        if (howlRef.current) {
                            howlRef.current.unload();
                        }
                        // Trigger re-render by updating a state
                        setIsBuffering(true);
                    }, 500 * (retryCount + 1)); // Increasing delay
                    return;
                }

                // Max retries reached, show error
                setIsBuffering(false);
                setLoadError('Failed to load audio file. The file may be too large or the format is unsupported.');
                onError?.('Failed to load audio file');
            },
            onplayerror: (_id, error) => {
                console.error('Howler play error:', error);
                // Try to unlock and play again
                howl.once('unlock', () => {
                    howl.play();
                });
            },
            onplay: () => {
                setPlayback(prev => ({ ...prev, isPlaying: true }));
                // Start time update interval
                updateIntervalRef.current = window.setInterval(() => {
                    if (howlRef.current) {
                        const seek = howlRef.current.seek() as number;
                        setPlayback(prev => ({
                            ...prev,
                            currentTime: seek,
                        }));
                    }
                }, 100);
            },
            onpause: () => {
                setPlayback(prev => ({ ...prev, isPlaying: false }));
                if (updateIntervalRef.current) {
                    clearInterval(updateIntervalRef.current);
                }
            },
            onstop: () => {
                setPlayback(prev => ({ ...prev, isPlaying: false, currentTime: 0 }));
                if (updateIntervalRef.current) {
                    clearInterval(updateIntervalRef.current);
                }
            },
            onend: () => {
                if (!playback.isLooping) {
                    setPlayback(prev => ({ ...prev, isPlaying: false }));
                    if (updateIntervalRef.current) {
                        clearInterval(updateIntervalRef.current);
                    }
                }
            },
            onseek: () => {
                if (howlRef.current) {
                    setPlayback(prev => ({
                        ...prev,
                        currentTime: howlRef.current!.seek() as number,
                    }));
                }
            },
        });

        howlRef.current = howl;

        // Cleanup on unmount
        return () => {
            if (updateIntervalRef.current) {
                clearInterval(updateIntervalRef.current);
            }
            if (howlRef.current) {
                howlRef.current.unload();
                howlRef.current = null;
            }
            // Reset analyser refs so they get recreated on next audio load
            analyserRef.current = null;
            sourceNodeRef.current = null;
            connectedElementRef.current = null;
        };
    }, [audioSrc, file.name, retryCount]); // Reinitialize when source changes or on retry

    // Update volume
    useEffect(() => {
        if (howlRef.current) {
            howlRef.current.volume(playback.isMuted ? 0 : playback.volume);
        }
    }, [playback.volume, playback.isMuted]);

    // Update loop
    useEffect(() => {
        if (howlRef.current) {
            howlRef.current.loop(playback.isLooping);
        }
    }, [playback.isLooping]);

    // Update playback rate
    useEffect(() => {
        if (howlRef.current) {
            howlRef.current.rate(playback.playbackRate);
        }
    }, [playback.playbackRate]);

    // Playback controls
    const togglePlay = useCallback(() => {
        if (!howlRef.current) return;

        if (playback.isPlaying) {
            howlRef.current.pause();
        } else {
            howlRef.current.play();
        }
    }, [playback.isPlaying]);

    const seek = useCallback((time: number) => {
        if (howlRef.current) {
            howlRef.current.seek(time);
            setPlayback(prev => ({ ...prev, currentTime: time }));
        }
    }, []);

    const setVolume = useCallback((volume: number) => {
        setPlayback(prev => ({ ...prev, volume, isMuted: volume === 0 }));
    }, []);

    const toggleMute = useCallback(() => {
        setPlayback(prev => ({ ...prev, isMuted: !prev.isMuted }));
    }, []);

    const toggleLoop = useCallback(() => {
        setPlayback(prev => ({ ...prev, isLooping: !prev.isLooping }));
    }, []);

    const setPlaybackRate = useCallback((rate: number) => {
        setPlayback(prev => ({ ...prev, playbackRate: rate }));
    }, []);

    const skipBackward = useCallback(() => {
        seek(Math.max(0, playback.currentTime - 5));
    }, [playback.currentTime, seek]);

    const skipForward = useCallback(() => {
        seek(Math.min(playback.duration, playback.currentTime + 5));
    }, [playback.currentTime, playback.duration, seek]);

    // Keyboard shortcuts
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            if (!containerRef.current?.contains(document.activeElement) &&
                document.activeElement !== document.body) {
                return;
            }

            switch (e.key.toLowerCase()) {
                case ' ':
                    e.preventDefault();
                    togglePlay();
                    break;
                case 'arrowleft':
                    e.preventDefault();
                    skipBackward();
                    break;
                case 'arrowright':
                    e.preventDefault();
                    skipForward();
                    break;
                case 'arrowup':
                    e.preventDefault();
                    setVolume(Math.min(1, playback.volume + 0.1));
                    break;
                case 'arrowdown':
                    e.preventDefault();
                    setVolume(Math.max(0, playback.volume - 0.1));
                    break;
                case 'e':
                    e.preventDefault();
                    setShowMixer(prev => !prev);
                    break;
                case 'm':
                    e.preventDefault();
                    toggleMute();
                    break;
                case 'l':
                    e.preventDefault();
                    toggleLoop();
                    break;
                case 'c':
                    e.preventDefault();
                    setCyberMode(prev => !prev);
                    break;
            }
        };

        document.addEventListener('keydown', handleKeyDown);
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [togglePlay, skipBackward, skipForward, setVolume, playback.volume, toggleMute, toggleLoop]);

    // Mouse wheel volume control
    const handleWheel = useCallback((e: React.WheelEvent) => {
        e.preventDefault();
        const delta = e.deltaY > 0 ? -0.02 : 0.02;
        setVolume(Math.max(0, Math.min(1, playback.volume + delta)));
    }, [playback.volume, setVolume]);

    // Render loading state
    if (!audioSrc) {
        return (
            <div className={`flex items-center justify-center h-full bg-gray-900 ${className}`}>
                <div className="text-gray-500">No audio data available</div>
            </div>
        );
    }

    return (
        <div
            ref={containerRef}
            className={`relative flex flex-col h-full bg-gray-900 ${className}`}
            tabIndex={0}
            onWheel={handleWheel}
        >
            {/* Visualizer area - expands in fullscreen mode */}
            <div className={`flex-1 flex items-center justify-center ${isFullscreen ? 'p-0' : 'p-6'}`}>
                <div className={`w-full ${isFullscreen ? 'h-full' : 'max-w-4xl h-72'} relative`}>
                    {/* Visualizer */}
                    <AudioVisualizer
                        analyser={analyserRef.current}
                        mode={visualizerMode}
                        isPlaying={playback.isPlaying}
                        cyberMode={cyberMode}
                        className={isFullscreen ? '' : 'rounded-xl'}
                    />

                    {/* Buffering overlay */}
                    {isBuffering && !loadError && (
                        <div className="absolute inset-0 flex items-center justify-center bg-black/50 rounded-xl">
                            <div className="flex flex-col items-center gap-2">
                                <Loader2 size={32} className="text-cyan-400 animate-spin" />
                                <span className="text-sm text-gray-300">
                                    {retryCount > 0 ? `Retrying... (${retryCount}/${maxRetries})` : 'Loading audio...'}
                                </span>
                            </div>
                        </div>
                    )}

                    {/* Error overlay with retry button */}
                    {loadError && (
                        <div className="absolute inset-0 flex items-center justify-center bg-black/70 rounded-xl">
                            <div className="flex flex-col items-center gap-3 text-center px-4">
                                <AlertTriangle size={40} className="text-yellow-500" />
                                <span className="text-sm text-red-400">{loadError}</span>
                                <button
                                    onClick={() => {
                                        setLoadError(null);
                                        setRetryCount(0);
                                        setIsBuffering(true);
                                    }}
                                    className="flex items-center gap-2 px-4 py-2 bg-cyan-600 hover:bg-cyan-500 text-white rounded-lg transition-colors"
                                >
                                    <RefreshCw size={16} />
                                    Try Again
                                </button>
                            </div>
                        </div>
                    )}

                    {/* Top controls overlay */}
                    <div className="absolute top-2 right-2 flex items-center gap-2">
                        {/* Fullscreen toggle */}
                        <button
                            onClick={() => {
                                const container = containerRef.current;
                                if (!container) return;
                                if (!document.fullscreenElement) {
                                    container.requestFullscreen();
                                    setIsFullscreen(true);
                                } else {
                                    document.exitFullscreen();
                                    setIsFullscreen(false);
                                }
                            }}
                            className="p-1.5 rounded bg-black/40 hover:bg-black/60 text-white/70 hover:text-white transition-colors"
                            title={isFullscreen ? 'Exit Fullscreen' : 'Fullscreen VJ Mode'}
                        >
                            {isFullscreen ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
                        </button>
                        {/* Cyber Mode toggle */}
                        <button
                            onClick={() => setCyberMode(prev => !prev)}
                            className={`p-2 rounded-lg transition-all ${cyberMode
                                ? 'bg-cyan-500/30 text-cyan-400 shadow-lg shadow-cyan-500/20'
                                : 'bg-gray-800/80 hover:bg-gray-700 text-gray-400'
                                }`}
                            title="Toggle Cyber Mode (C)"
                        >
                            <Zap size={16} className={cyberMode ? 'animate-pulse' : ''} />
                        </button>

                        {/* Visualizer mode dropdown */}
                        <div className="relative">
                            <button
                                onClick={() => setShowVisualizerMenu(prev => !prev)}
                                className="flex items-center gap-1 px-2 py-1.5 bg-gray-800/80 hover:bg-gray-700 rounded-lg transition-colors"
                                title="Change visualizer mode"
                            >
                                {VISUALIZER_MODES.find(m => m.value === visualizerMode)?.icon}
                                <ChevronDown size={12} className="text-gray-400" />
                            </button>

                            {showVisualizerMenu && (
                                <div className="absolute top-full right-0 mt-1 bg-gray-800 border border-gray-700 rounded-lg shadow-xl py-1 z-10">
                                    {VISUALIZER_MODES.map((mode) => (
                                        <button
                                            key={mode.value}
                                            onClick={() => {
                                                setVisualizerMode(mode.value);
                                                setShowVisualizerMenu(false);
                                            }}
                                            className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-gray-700 transition-colors ${visualizerMode === mode.value ? 'text-cyan-400' : 'text-white'
                                                }`}
                                        >
                                            {mode.icon}
                                            {mode.label}
                                        </button>
                                    ))}
                                </div>
                            )}
                        </div>
                    </div>

                    {/* AeroPlayer branding */}
                    <div className={`absolute bottom-2 right-2 text-xs font-mono transition-all ${cyberMode ? 'text-cyan-400/70 animate-pulse' : 'text-gray-600 opacity-50'
                        }`}>
                        AeroPlayer{cyberMode ? ' // CYBER' : ''}
                    </div>
                </div>
            </div>

            {/* Track info */}
            <div className="text-center px-4 py-2">
                <h3
                    className={`text-lg font-medium truncate transition-all ${cyberMode ? 'text-cyan-300' : 'text-white'} ${playback.isPlaying ? 'drop-shadow-[0_0_8px_rgba(147,197,253,0.5)]' : ''
                        }`}
                    style={playback.isPlaying ? { textShadow: '0 0 10px rgba(147, 197, 253, 0.6)' } : {}}
                >
                    {metadata?.title || file.name}
                </h3>
                {metadata?.artist && (
                    <p className="text-sm text-gray-400">
                        {metadata.artist}
                        {metadata.album && ` • ${metadata.album}`}
                    </p>
                )}
            </div>

            {/* Progress bar */}
            <div className="px-6 py-2">
                <div
                    className={`relative bg-gray-700 rounded-full cursor-pointer group transition-all ${playback.isPlaying ? 'h-2 shadow-lg shadow-purple-500/20' : 'h-1.5'
                        }`}
                    style={playback.isPlaying ? {
                        animation: 'pulse-glow 2s ease-in-out infinite',
                    } : {}}
                    onClick={(e) => {
                        const rect = e.currentTarget.getBoundingClientRect();
                        const percent = (e.clientX - rect.left) / rect.width;
                        seek(percent * playback.duration);
                    }}
                >
                    {/* Buffered */}
                    <div
                        className="absolute h-full bg-gray-600 rounded-full"
                        style={{ width: `${playback.bufferedPercent}%` }}
                    />
                    {/* Progress */}
                    <div
                        className={`absolute h-full rounded-full transition-all ${cyberMode
                            ? 'bg-gradient-to-r from-cyan-400 via-purple-500 to-pink-500'
                            : 'bg-gradient-to-r from-blue-500 to-purple-500'
                            } ${playback.isPlaying ? 'shadow-md shadow-purple-500/40' : ''}`}
                        style={{ width: `${playback.duration > 0 ? (playback.currentTime / playback.duration) * 100 : 0}%` }}
                    />
                    {/* Thumb */}
                    <div
                        className={`absolute top-1/2 -translate-y-1/2 w-4 h-4 rounded-full shadow-lg opacity-0 group-hover:opacity-100 transition-all ${cyberMode ? 'bg-cyan-400 shadow-cyan-400/50' : 'bg-white shadow-white/30'}`}
                        style={{ left: `calc(${playback.duration > 0 ? (playback.currentTime / playback.duration) * 100 : 0}% - 8px)` }}
                    />
                </div>
                <div className="flex justify-between mt-1 text-xs text-gray-500 font-mono">
                    <span>{formatDuration(playback.currentTime)}</span>
                    <span>{formatDuration(playback.duration)}</span>
                </div>
            </div>

            {/* Controls */}
            <div className="flex items-center justify-center gap-4 py-4">
                {/* Skip backward */}
                <button
                    onClick={skipBackward}
                    className="p-2 hover:bg-gray-800 rounded-full transition-colors"
                    title="Skip -5s (←)"
                >
                    <SkipBack size={20} className="text-gray-400" />
                </button>

                {/* Play/Pause */}
                <button
                    onClick={togglePlay}
                    disabled={isBuffering}
                    className={`p-4 rounded-full shadow-lg transition-all disabled:opacity-70 ${cyberMode
                        ? 'bg-gradient-to-br from-cyan-500 to-purple-600 hover:from-cyan-400 hover:to-purple-500 shadow-cyan-500/30'
                        : 'bg-gradient-to-br from-blue-500 to-purple-600 hover:from-blue-400 hover:to-purple-500'
                        }`}
                >
                    {isBuffering ? (
                        <Loader2 size={24} className="text-white animate-spin" />
                    ) : playback.isPlaying ? (
                        <Pause size={24} className="text-white" />
                    ) : (
                        <Play size={24} className="text-white ml-0.5" />
                    )}
                </button>

                {/* Skip forward */}
                <button
                    onClick={skipForward}
                    className="p-2 hover:bg-gray-800 rounded-full transition-colors"
                    title="Skip +5s (→)"
                >
                    <SkipForward size={20} className="text-gray-400" />
                </button>
            </div>

            {/* Secondary controls */}
            <div className="flex items-center justify-between px-6 py-3 border-t border-gray-800">
                {/* Volume */}
                <div className="flex items-center gap-2 w-36">
                    <button
                        onClick={toggleMute}
                        className="p-1 hover:bg-gray-800 rounded transition-colors"
                        title="Mute (M)"
                    >
                        {playback.isMuted || playback.volume === 0 ? (
                            <VolumeX size={18} className="text-gray-500" />
                        ) : (
                            <Volume2 size={18} className="text-gray-400" />
                        )}
                    </button>
                    <input
                        type="range"
                        min="0"
                        max="1"
                        step="0.01"
                        value={playback.isMuted ? 0 : playback.volume}
                        onChange={(e) => setVolume(parseFloat(e.target.value))}
                        className="flex-1 h-1 appearance-none bg-gray-700 rounded-full cursor-pointer"
                    />
                </div>

                {/* Center controls */}
                <div className="flex items-center gap-2">
                    {/* Loop */}
                    <button
                        onClick={toggleLoop}
                        className={`p-2 rounded-lg transition-colors ${playback.isLooping
                            ? (cyberMode ? 'bg-cyan-600 text-white' : 'bg-blue-600 text-white')
                            : 'text-gray-400 hover:bg-gray-800'
                            }`}
                        title="Toggle Loop (L)"
                    >
                        <Repeat size={16} />
                    </button>

                    {/* Playback speed */}
                    <button
                        onClick={() => {
                            const rates = [0.5, 0.75, 1, 1.25, 1.5, 2];
                            const currentIndex = rates.indexOf(playback.playbackRate);
                            const nextIndex = (currentIndex + 1) % rates.length;
                            setPlaybackRate(rates[nextIndex]);
                        }}
                        className="flex items-center gap-1 px-2 py-1 bg-gray-800 hover:bg-gray-700 rounded-lg transition-colors"
                        title="Playback Speed"
                    >
                        <Gauge size={14} className="text-gray-400" />
                        <span className="text-xs text-gray-300 font-mono w-8">
                            {playback.playbackRate}x
                        </span>
                    </button>
                </div>

                {/* Mixer toggle */}
                <div className="w-36 flex justify-end">
                    {!showMixer && (
                        <AudioMixer
                            state={eqState}
                            onStateChange={setEQState}
                            isExpanded={false}
                            onToggleExpand={() => setShowMixer(true)}
                        />
                    )}
                </div>
            </div>

            {/* Expanded mixer panel - overlay style */}
            {showMixer && (
                <div className="absolute bottom-0 right-0 z-20 w-80 m-2">
                    <AudioMixer
                        state={eqState}
                        onStateChange={setEQState}
                        isExpanded={true}
                        onToggleExpand={() => setShowMixer(false)}
                    />
                </div>
            )}

            {/* Keyboard shortcuts hint */}
            <div className="text-center text-xs text-gray-600 pb-2">
                Space: Play/Pause • ←→: Skip • ↑↓: Volume • E: EQ • C: Cyber Mode
            </div>
        </div>
    );
};

export default AudioPlayer;
