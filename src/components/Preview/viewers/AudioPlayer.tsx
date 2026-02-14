/**
 * Audio Player Component - AeroPlayer CYBER EDITION
 *
 * Full-featured audio player powered by native HTML5 Audio + Web Audio API:
 * - Native browser streaming (no buffer issues with large files)
 * - Real-time 10-band graphic equalizer via BiquadFilterNode
 * - Stereo balance via StereoPannerNode
 * - Real-time visualizer with multiple modes
 * - Keyboard shortcuts
 * - Playback speed control & Loop toggle
 */

import React, { useRef, useState, useEffect, useCallback } from 'react';
import {
    Play, Pause, SkipBack, SkipForward, Volume2, VolumeX,
    Repeat, Gauge, Activity, BarChart2, Circle, Waves, Zap, ChevronDown, Loader2,
    Sparkles, Flame, Maximize2, Minimize2, Eye, RefreshCw, AlertTriangle
} from 'lucide-react';
import { ViewerBaseProps, PlaybackState, EqualizerState, MediaMetadata } from '../types';
import { formatDuration } from '../utils/fileTypes';
import { AudioVisualizer, VisualizerMode } from '../controls/AudioVisualizer';
import { WebGLVisualizer } from '../controls/WebGLVisualizer';
import type { WebGLShaderName } from '../controls/shaders';
import { AudioMixer, EQ_BANDS } from '../controls/AudioMixer';
import { logger } from '../../../utils/logger';
import { useI18n } from '../../../i18n';

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

// Visualizer mode options - will be translated inline in component
const getVisualizerModes = (t: (key: string) => string): { value: VisualizerMode; label: string; icon: React.ReactNode }[] => [
    { value: 'bars', label: t('preview.audio.visualizer.bars'), icon: <BarChart2 size={14} /> },
    { value: 'waveform', label: t('preview.audio.visualizer.waveform'), icon: <Activity size={14} /> },
    { value: 'radial', label: t('preview.audio.visualizer.radial'), icon: <Circle size={14} /> },
    { value: 'spectrum', label: t('preview.audio.visualizer.spectrum'), icon: <Waves size={14} /> },
    { value: 'fractal', label: t('preview.audio.visualizer.fractal'), icon: <Sparkles size={14} /> },
    { value: 'vortex', label: t('preview.audio.visualizer.vortex'), icon: <Zap size={14} /> },
    { value: 'plasma', label: t('preview.audio.visualizer.plasma'), icon: <Flame size={14} /> },
    { value: 'kaleidoscope', label: t('preview.audio.visualizer.kaleidoscope'), icon: <Eye size={14} /> },
];

// WebGL shader visualizer modes
const GL_MODES: { name: string; shader: WebGLShaderName }[] = [
    { name: 'GL: Wave Glitch', shader: 'wave_glitch' },
    { name: 'GL: VHS', shader: 'glitch_vhs' },
    { name: 'GL: Mandelbrot', shader: 'fractal_mandelbrot' },
    { name: 'GL: Tunnel', shader: 'raymarch_tunnel' },
    { name: 'GL: Metaball', shader: 'metaball_pulse' },
    { name: 'GL: Particles', shader: 'particles_explosion' },
];

// EQ frequency values matching AudioMixer bands
const EQ_FREQUENCIES = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const MIN_START_BUFFER_SECONDS = 6.0;

// Global map to track audio elements already connected to MediaElementSource
// Once connected, an audio element cannot be reconnected to a new source node
const connectedAudioElements = new WeakMap<HTMLMediaElement, {
    source: MediaElementAudioSourceNode;
    analyser: AnalyserNode;
    eqNodes: BiquadFilterNode[];
    panner: StereoPannerNode;
}>();

export const AudioPlayer: React.FC<AudioPlayerProps> = ({
    file,
    onError,
    className = '',
}) => {
    const { t } = useI18n();

    // Refs
    const audioRef = useRef<HTMLAudioElement>(null);
    const audioCtxRef = useRef<AudioContext | null>(null);
    const sourceNodeRef = useRef<MediaElementAudioSourceNode | null>(null);
    const analyserRef = useRef<AnalyserNode | null>(null);
    const eqNodesRef = useRef<BiquadFilterNode[]>([]);
    const pannerRef = useRef<StereoPannerNode | null>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const graphBuiltRef = useRef(false);
    const retryPlayOnCanPlayRef = useRef(false);
    const warmupDoneRef = useRef(false);
    const warmupInProgressRef = useRef(false);
    const suppressPlaybackUiRef = useRef(false);
    const pendingUserPlayRef = useRef(false);

    // Get localized visualizer modes
    const VISUALIZER_MODES = getVisualizerModes(t);

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
    const [glShader, setGlShader] = useState<WebGLShaderName | null>(null);
    const [cyberMode, setCyberMode] = useState(false);
    const [showMixer, setShowMixer] = useState(false);
    const [showVisualizerMenu, setShowVisualizerMenu] = useState(false);
    const [isAudioReady, setIsAudioReady] = useState(false);
    const [isBuffering, setIsBuffering] = useState(true);
    const [isPlayQueued, setIsPlayQueued] = useState(false);
    const [isFullscreen, setIsFullscreen] = useState(false);
    const [loadError, setLoadError] = useState<string | null>(null);
    const [retryCount, setRetryCount] = useState(0);
    const maxRetries = 3;

    // Audio source URL
    const audioSrc = file.blobUrl || file.content as string || '';

    // Build Web Audio graph once after mount
    const buildAudioGraph = useCallback(() => {
        const audioEl = audioRef.current;
        if (!audioEl || graphBuiltRef.current) return;

        // Reuse existing connection if element was already connected
        const existing = connectedAudioElements.get(audioEl);
        if (existing) {
            sourceNodeRef.current = existing.source;
            analyserRef.current = existing.analyser;
            eqNodesRef.current = existing.eqNodes;
            pannerRef.current = existing.panner;
            graphBuiltRef.current = true;
            logger.debug('Reusing existing Web Audio graph connection');
            return;
        }

        // Create or reuse AudioContext
        let ctx = audioCtxRef.current;
        if (!ctx) {
            ctx = new AudioContext();
            audioCtxRef.current = ctx;
        }

        try {
            // Source node from <audio> element
            const source = ctx.createMediaElementSource(audioEl);

            // 10-band EQ: BiquadFilterNode chain (peaking type)
            const eqNodes: BiquadFilterNode[] = EQ_FREQUENCIES.map((freq) => {
                const filter = ctx!.createBiquadFilter();
                filter.type = 'peaking';
                filter.frequency.value = freq;
                filter.Q.value = 1.4;
                filter.gain.value = 0;
                return filter;
            });

            // Stereo balance
            const panner = ctx.createStereoPanner();
            panner.pan.value = 0;

            // Analyser for visualizer (fftSize 512 = 256 bins)
            const analyser = ctx.createAnalyser();
            analyser.fftSize = 512;
            analyser.smoothingTimeConstant = 0.82;

            // Connect chain: source -> eq[0] -> eq[1] -> ... -> eq[9] -> panner -> analyser -> destination
            source.connect(eqNodes[0]);
            for (let i = 0; i < eqNodes.length - 1; i++) {
                eqNodes[i].connect(eqNodes[i + 1]);
            }
            eqNodes[eqNodes.length - 1].connect(panner);
            panner.connect(analyser);
            analyser.connect(ctx.destination);

            // Store in WeakMap to prevent reconnection
            connectedAudioElements.set(audioEl, { source, analyser, eqNodes, panner });

            // Store in refs
            sourceNodeRef.current = source;
            analyserRef.current = analyser;
            eqNodesRef.current = eqNodes;
            pannerRef.current = panner;
            graphBuiltRef.current = true;

            logger.debug('Web Audio graph built: source -> EQ[10] -> panner -> analyser -> destination');
        } catch (err: any) {
            if (err.name === 'InvalidStateError') {
                logger.warn('Audio element already connected externally');
            } else {
                console.error('Failed to build audio graph:', err);
            }
        }
    }, []);

    // Set audio source when it changes
    useEffect(() => {
        const audioEl = audioRef.current;
        if (!audioEl || !audioSrc) return;

        // Reset state for new source
        warmupDoneRef.current = false;
        warmupInProgressRef.current = false;
        suppressPlaybackUiRef.current = false;
        pendingUserPlayRef.current = false;
        setIsAudioReady(false);
        setIsBuffering(true);
        setIsPlayQueued(false);
        setLoadError(null);

        audioEl.src = audioSrc;
        audioEl.volume = playback.volume;
        audioEl.load();

        logger.debug(`Audio source set: ${file.name}`);

        // Resume AudioContext if suspended (autoplay policy)
        if (audioCtxRef.current?.state === 'suspended') {
            audioCtxRef.current.resume().catch(() => {});
        }
    }, [audioSrc, file.name]);

    // Update volume
    useEffect(() => {
        if (audioRef.current) {
            audioRef.current.volume = playback.isMuted ? 0 : playback.volume;
        }
    }, [playback.volume, playback.isMuted]);

    // Update loop
    useEffect(() => {
        if (audioRef.current) {
            audioRef.current.loop = playback.isLooping;
        }
    }, [playback.isLooping]);

    // Update playback rate
    useEffect(() => {
        if (audioRef.current) {
            audioRef.current.playbackRate = playback.playbackRate;
        }
    }, [playback.playbackRate]);

    // Cleanup on unmount
    useEffect(() => {
        return () => {
            if (audioRef.current) {
                audioRef.current.pause();
                audioRef.current.removeAttribute('src');
                audioRef.current.load();
            }
            if (audioCtxRef.current && audioCtxRef.current.state !== 'closed') {
                audioCtxRef.current.close();
            }
        };
    }, []);

    const runSilentWarmup = useCallback(async () => {
        const audioEl = audioRef.current;
        if (!audioEl || warmupDoneRef.current || warmupInProgressRef.current) return;
        if (!audioEl.paused || retryPlayOnCanPlayRef.current) return;

        warmupInProgressRef.current = true;
        suppressPlaybackUiRef.current = true;

        const previousMuted = audioEl.muted;
        const previousVolume = audioEl.volume;

        logger.debug('[AudioDebug] warmup start');

        try {
            audioEl.muted = true;
            audioEl.volume = 0;

            await audioEl.play();
            await new Promise(resolve => window.setTimeout(resolve, 180));
            audioEl.pause();

            if (audioEl.currentTime > 0) {
                audioEl.currentTime = 0;
            }

            warmupDoneRef.current = true;
            logger.debug('[AudioDebug] warmup success');
        } catch (err: any) {
            logger.debug(
                `[AudioDebug] warmup skipped name=${err?.name ?? 'unknown'} message=${err?.message ?? 'n/a'}`
            );
        } finally {
            audioEl.muted = previousMuted;
            audioEl.volume = playback.isMuted ? 0 : playback.volume;
            suppressPlaybackUiRef.current = false;
            warmupInProgressRef.current = false;
        }
    }, [playback.isMuted, playback.volume]);

    const getBufferedAheadSeconds = useCallback((audioEl: HTMLAudioElement) => {
        if (audioEl.buffered.length === 0) return 0;

        const currentTime = audioEl.currentTime;
        for (let i = 0; i < audioEl.buffered.length; i++) {
            const start = audioEl.buffered.start(i);
            const end = audioEl.buffered.end(i);
            if (currentTime >= start && currentTime <= end) {
                return Math.max(0, end - currentTime);
            }
        }

        return 0;
    }, []);

    const attemptStartQueuedPlay = useCallback(async (trigger: string) => {
        const audioEl = audioRef.current;
        if (!audioEl || !pendingUserPlayRef.current || !audioEl.paused) return;

        const bufferedAhead = getBufferedAheadSeconds(audioEl);
        const hasMinimumBuffer =
            audioEl.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA && bufferedAhead >= MIN_START_BUFFER_SECONDS;

        if (!hasMinimumBuffer) {
            setIsBuffering(true);
            logger.debug(
                `[AudioDebug] queued play waiting buffer trigger=${trigger} readyState=${audioEl.readyState} bufferedAhead=${bufferedAhead.toFixed(3)}`
            );
            return;
        }

        if (audioCtxRef.current?.state === 'suspended') {
            try {
                await audioCtxRef.current.resume();
            } catch {
                // Keep going: HTMLAudioElement playback may still succeed
            }
        }

        if (!graphBuiltRef.current) {
            buildAudioGraph();
        }

        try {
            await audioEl.play();
            pendingUserPlayRef.current = false;
            setIsPlayQueued(false);
            setIsBuffering(false);
            setLoadError(null);
            logger.debug(`[AudioDebug] queued play started trigger=${trigger}`);
        } catch (err: any) {
            logger.debug(
                `[AudioDebug] queued play rejected trigger=${trigger} name=${err?.name ?? 'unknown'} message=${err?.message ?? 'n/a'}`
            );

            if (err?.name === 'AbortError') {
                retryPlayOnCanPlayRef.current = true;
                setIsBuffering(true);
                return;
            }

            pendingUserPlayRef.current = false;
            setIsPlayQueued(false);
            if (err?.name !== 'NotAllowedError') {
                setLoadError(t('preview.audio.playbackFailed'));
            }
        }
    }, [buildAudioGraph, getBufferedAheadSeconds, t]);

    // Playback controls
    const togglePlay = useCallback(async () => {
        const audioEl = audioRef.current;
        if (!audioEl) return;

        const delay = (ms: number) => new Promise(resolve => window.setTimeout(resolve, ms));

        if (warmupInProgressRef.current) {
            logger.debug('[AudioDebug] interrupting warmup due to user play request');
            suppressPlaybackUiRef.current = false;
            warmupInProgressRef.current = false;
            try {
                audioEl.pause();
            } catch {
                // ignore
            }
            if (audioEl.currentTime > 0) {
                audioEl.currentTime = 0;
            }
            audioEl.muted = playback.isMuted;
            audioEl.volume = playback.isMuted ? 0 : playback.volume;
        }

        // Use actual DOM state, not React state
        if (audioEl.paused) {
            retryPlayOnCanPlayRef.current = false;
            pendingUserPlayRef.current = true;
            setIsPlayQueued(true);
            setLoadError(null);

            logger.debug(
                `[AudioDebug] togglePlay->requestPlay readyState=${audioEl.readyState} networkState=${audioEl.networkState} src=${Boolean(audioEl.src)}`
            );

            if (!audioEl.src && audioSrc) {
                audioEl.src = audioSrc;
                logger.debug('[AudioDebug] audio src assigned during togglePlay');
            }

            setIsBuffering(true);
            logger.debug('[AudioDebug] play queued while buffering');

            await attemptStartQueuedPlay('togglePlay');
        } else {
            retryPlayOnCanPlayRef.current = false;
            pendingUserPlayRef.current = false;
            setIsPlayQueued(false);
            logger.debug('[AudioDebug] togglePlay->pause');
            audioEl.pause();
        }
    }, [attemptStartQueuedPlay, audioSrc, playback.isMuted, playback.volume]);

    const seek = useCallback((time: number) => {
        if (audioRef.current) {
            audioRef.current.currentTime = time;
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

    // Audio element event handlers
    const handleLoadedMetadata = useCallback(() => {
        if (audioRef.current) {
            logger.debug(
                `[AudioDebug] loadedmetadata duration=${audioRef.current.duration} readyState=${audioRef.current.readyState}`
            );
            setIsAudioReady(true);
            setIsBuffering(false);
            setLoadError(null);
            setRetryCount(0);
            setPlayback(prev => ({
                ...prev,
                duration: audioRef.current!.duration,
            }));
            setMetadata({
                title: file.name.replace(/\.[^/.]+$/, ''),
            });

            // Build audio graph now that audio element has valid source
            if (!graphBuiltRef.current) {
                buildAudioGraph();
            }
        }
    }, [file.name, buildAudioGraph]);

    const handleCanPlay = useCallback(() => {
        const audioEl = audioRef.current;
        logger.debug(
            `[AudioDebug] canplay readyState=${audioEl?.readyState ?? -1} currentTime=${audioEl?.currentTime ?? 0}`
        );
        setIsAudioReady(true);
        if (!pendingUserPlayRef.current) {
            setIsBuffering(false);
        }

        if (pendingUserPlayRef.current) {
            void attemptStartQueuedPlay('canplay');
            return;
        }

        if (!retryPlayOnCanPlayRef.current) {
            void runSilentWarmup();
        }

        if (audioEl && retryPlayOnCanPlayRef.current && audioEl.paused) {
            retryPlayOnCanPlayRef.current = false;
            logger.debug('[AudioDebug] executing one-shot play retry on canplay');

            void audioEl.play()
                .then(() => {
                    setLoadError(null);
                    logger.debug('[AudioDebug] canplay retry success');
                })
                .catch((err: any) => {
                    logger.debug(
                        `[AudioDebug] canplay retry rejected name=${err?.name ?? 'unknown'} message=${err?.message ?? 'n/a'}`
                    );

                    if (err?.name !== 'NotAllowedError' && err?.name !== 'AbortError') {
                        setLoadError(t('preview.audio.playbackFailed'));
                    }
                });
        }
    }, [attemptStartQueuedPlay, runSilentWarmup, t]);

    const handleTimeUpdate = useCallback(() => {
        if (audioRef.current) {
            setPlayback(prev => ({
                ...prev,
                currentTime: audioRef.current!.currentTime,
            }));
        }
    }, []);

    const handleProgress = useCallback(() => {
        const audioEl = audioRef.current;
        if (audioEl && audioEl.buffered.length > 0) {
            const bufferedEnd = audioEl.buffered.end(audioEl.buffered.length - 1);
            const duration = audioEl.duration;
            setPlayback(prev => ({
                ...prev,
                bufferedPercent: duration > 0 ? (bufferedEnd / duration) * 100 : 0,
            }));

            if (pendingUserPlayRef.current && audioEl.paused) {
                void attemptStartQueuedPlay('progress');
            }
        }
    }, [attemptStartQueuedPlay]);

    const handleError = useCallback(() => {
        const mediaErrorCode = audioRef.current?.error?.code;
        const mediaErrorMessage = audioRef.current?.error?.message;
        logger.debug(
            `[AudioDebug] audio error code=${mediaErrorCode ?? 'n/a'} message=${mediaErrorMessage ?? 'n/a'} retry=${retryCount}/${maxRetries}`
        );

        if (retryCount < maxRetries) {
            logger.debug(`Retrying audio load (${retryCount + 1}/${maxRetries})...`);
            setRetryCount(prev => prev + 1);
            setTimeout(() => {
                if (audioRef.current) {
                    audioRef.current.load();
                }
            }, 500 * (retryCount + 1));
            return;
        }
        retryPlayOnCanPlayRef.current = false;
        pendingUserPlayRef.current = false;
        setIsPlayQueued(false);
        setIsBuffering(false);
        setLoadError(t('preview.audio.loadFailed'));
        onError?.(t('preview.audio.loadFailed'));
    }, [retryCount, onError]);

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
                case 'v':
                    e.preventDefault();
                    // Cycle through all 14 modes: 8 canvas + 6 GL
                    if (!glShader) {
                        const currentCanvasIdx = VISUALIZER_MODES.findIndex(m => m.value === visualizerMode);
                        if (currentCanvasIdx < VISUALIZER_MODES.length - 1) {
                            setVisualizerMode(VISUALIZER_MODES[currentCanvasIdx + 1].value);
                        } else {
                            // Switch to first GL mode
                            setGlShader(GL_MODES[0].shader);
                        }
                    } else {
                        const currentGlIdx = GL_MODES.findIndex(m => m.shader === glShader);
                        if (currentGlIdx < GL_MODES.length - 1) {
                            setGlShader(GL_MODES[currentGlIdx + 1].shader);
                        } else {
                            // Wrap back to first canvas mode
                            setGlShader(null);
                            setVisualizerMode('bars');
                        }
                    }
                    break;
            }
        };

        document.addEventListener('keydown', handleKeyDown);
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [togglePlay, skipBackward, skipForward, setVolume, playback.volume, toggleMute, toggleLoop, visualizerMode, glShader]);

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
                <div className="text-gray-500">{t('preview.common.noData')}</div>
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
            {/* Hidden audio element - always present for stable Web Audio graph */}
            <audio
                ref={audioRef}
                preload="auto"
                onLoadedMetadata={handleLoadedMetadata}
                onTimeUpdate={handleTimeUpdate}
                onPlay={() => {
                    if (!suppressPlaybackUiRef.current) {
                        setPlayback(prev => ({ ...prev, isPlaying: true }));
                    }
                }}
                onPause={() => {
                    if (!suppressPlaybackUiRef.current) {
                        setIsPlayQueued(false);
                        setPlayback(prev => ({ ...prev, isPlaying: false }));
                    }
                }}
                onEnded={() => {
                    if (!playback.isLooping) {
                        setPlayback(prev => ({ ...prev, isPlaying: false }));
                    }
                }}
                onProgress={handleProgress}
                onError={handleError}
                onWaiting={() => setIsBuffering(true)}
                onCanPlay={handleCanPlay}
                style={{ display: 'none' }}
            />

            {/* Visualizer area - expands in fullscreen mode */}
            <div className={`flex-1 flex items-center justify-center ${isFullscreen ? 'p-0' : 'p-6'}`}>
                <div className={`w-full ${isFullscreen ? 'h-full' : 'max-w-4xl h-72'} relative`}>
                    {/* Visualizer */}
                    {glShader ? (
                        <WebGLVisualizer
                            analyser={analyserRef.current}
                            shader={glShader}
                            isPlaying={playback.isPlaying}
                            className={isFullscreen ? '' : 'rounded-xl'}
                            onContextLost={() => {
                                // Auto-fallback to Canvas 2D when WebGL context is lost
                                logger.warn('WebGL context lost — falling back to Canvas 2D');
                                setGlShader(null);
                            }}
                        />
                    ) : (
                        <AudioVisualizer
                            analyser={analyserRef.current}
                            mode={visualizerMode}
                            isPlaying={playback.isPlaying}
                            cyberMode={cyberMode}
                            className={isFullscreen ? '' : 'rounded-xl'}
                        />
                    )}

                    {/* Buffering overlay */}
                    {isBuffering && !loadError && (
                        <div className="absolute inset-0 flex items-center justify-center bg-black/50 rounded-xl">
                            <div className="flex flex-col items-center gap-2">
                                <Loader2 size={32} className="text-cyan-400 animate-spin" />
                                <span className="text-sm text-gray-300">
                                    {retryCount > 0 ? `${t('preview.audio.retrying')} (${retryCount}/${maxRetries})` : t('preview.audio.loading')}
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
                                        if (audioRef.current) {
                                            audioRef.current.load();
                                        }
                                    }}
                                    className="flex items-center gap-2 px-4 py-2 bg-cyan-600 hover:bg-cyan-500 text-white rounded-lg transition-colors"
                                >
                                    <RefreshCw size={16} />
                                    {t('preview.audio.tryAgain')}
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
                            title={isFullscreen ? t('preview.audio.exitFullscreen') : t('preview.audio.fullscreenVJ')}
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
                            title={t('preview.audio.cyberMode')}
                        >
                            <Zap size={16} className={cyberMode ? 'animate-pulse' : ''} />
                        </button>

                        {/* Visualizer mode dropdown */}
                        <div className="relative">
                            <button
                                onClick={() => setShowVisualizerMenu(prev => !prev)}
                                className="flex items-center gap-1 px-2 py-1.5 bg-gray-800/80 hover:bg-gray-700 rounded-lg transition-colors"
                                title={t('preview.audio.changeVisualizer')}
                            >
                                {glShader
                                    ? <span className="text-[10px] font-bold text-emerald-400 leading-none">GL</span>
                                    : VISUALIZER_MODES.find(m => m.value === visualizerMode)?.icon
                                }
                                <ChevronDown size={12} className="text-gray-400" />
                            </button>

                            {showVisualizerMenu && (
                                <div className="absolute top-full right-0 mt-1 bg-gray-800 border border-gray-700 rounded-lg shadow-xl py-1 z-10 min-w-[160px]">
                                    {/* Canvas 2D modes */}
                                    {VISUALIZER_MODES.map((mode) => (
                                        <button
                                            key={mode.value}
                                            onClick={() => {
                                                setVisualizerMode(mode.value);
                                                setGlShader(null);
                                                setShowVisualizerMenu(false);
                                            }}
                                            className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-gray-700 transition-colors ${!glShader && visualizerMode === mode.value ? 'text-cyan-400' : 'text-white'
                                                }`}
                                        >
                                            {mode.icon}
                                            {mode.label}
                                        </button>
                                    ))}
                                    {/* Separator */}
                                    <div className="border-t border-gray-600 my-1" />
                                    {/* WebGL shader modes */}
                                    {GL_MODES.map((mode) => (
                                        <button
                                            key={mode.shader}
                                            onClick={() => {
                                                setGlShader(mode.shader);
                                                setShowVisualizerMenu(false);
                                            }}
                                            className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-gray-700 transition-colors ${glShader === mode.shader ? 'text-emerald-400' : 'text-white'
                                                }`}
                                        >
                                            <span className="text-[9px] font-bold text-emerald-500 bg-emerald-500/10 px-1 rounded">GL</span>
                                            {mode.name.replace('GL: ', '')}
                                        </button>
                                    ))}
                                </div>
                            )}
                        </div>
                    </div>

                    {/* AeroPlayer branding */}
                    <div className={`absolute bottom-2 right-2 text-xs font-mono transition-all ${cyberMode ? 'text-cyan-400/70 animate-pulse' : 'text-gray-600 opacity-50'
                        }`}>
                        AeroPlayer{cyberMode ? ' // CYBER' : ''}{glShader ? ` // ${GL_MODES.find(m => m.shader === glShader)?.name || 'GL'}` : ''}
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
                    title={t('preview.audio.skipBackward')}
                >
                    <SkipBack size={20} className="text-gray-400" />
                </button>

                {/* Play/Pause */}
                <button
                    onClick={togglePlay}
                    className={`p-4 rounded-full shadow-lg transition-all ${cyberMode
                        ? 'bg-gradient-to-br from-cyan-500 to-purple-600 hover:from-cyan-400 hover:to-purple-500 shadow-cyan-500/30'
                        : 'bg-gradient-to-br from-blue-500 to-purple-600 hover:from-blue-400 hover:to-purple-500'
                        }`}
                >
                    {isBuffering || isPlayQueued ? (
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
                    title={t('preview.audio.skipForward')}
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
                        title={t('preview.audio.mute')}
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
                        title={t('preview.audio.loop')}
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
                        title={t('preview.audio.speed')}
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
                            eqNodes={eqNodesRef.current}
                            pannerNode={pannerRef.current}
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
                        eqNodes={eqNodesRef.current}
                        pannerNode={pannerRef.current}
                        isExpanded={true}
                        onToggleExpand={() => setShowMixer(false)}
                    />
                </div>
            )}

            {/* Keyboard shortcuts hint */}
            <div className="text-center text-xs text-gray-600 pb-2">
                {t('preview.audio.shortcuts')}
            </div>
        </div>
    );
};

export default AudioPlayer;
