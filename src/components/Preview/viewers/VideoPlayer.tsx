/**
 * Video Player Component
 * 
 * Full-featured video player with custom controls:
 * - Play/Pause, Seek, Volume
 * - Fullscreen toggle
 * - Picture-in-Picture mode
 * - Playback speed control
 * - Progress bar with buffering indicator
 * - Keyboard shortcuts (Space: play/pause, F: fullscreen, M: mute)
 * 
 * Uses HTML5 Video element with custom overlay controls.
 */

import React, { useRef, useState, useEffect, useCallback } from 'react';
import {
    Play, Pause, Volume2, VolumeX, Maximize, Minimize,
    PictureInPicture2, Gauge, SkipBack, SkipForward, Settings
} from 'lucide-react';
import { ViewerBaseProps, PlaybackState } from '../types';
import { formatDuration } from '../utils/fileTypes';
import { logger } from '../../../utils/logger';

interface VideoPlayerProps extends ViewerBaseProps {
    className?: string;
}

export const VideoPlayer: React.FC<VideoPlayerProps> = ({
    file,
    onError,
    className = '',
}) => {
    // Refs
    const videoRef = useRef<HTMLVideoElement>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const controlsTimeoutRef = useRef<number>(0);

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
    const [isFullscreen, setIsFullscreen] = useState(false);
    const [isPiP, setIsPiP] = useState(false);
    const [showControls, setShowControls] = useState(true);
    const [showSpeedMenu, setShowSpeedMenu] = useState(false);
    const [isVideoReady, setIsVideoReady] = useState(false);

    // Video source URL
    const videoSrc = file.blobUrl || file.content as string || '';
    logger.debug('[VideoPlayer] Source URL:', videoSrc, 'blobUrl:', file.blobUrl);

    // Auto-hide controls after 3 seconds of inactivity
    const resetControlsTimeout = useCallback(() => {
        setShowControls(true);
        clearTimeout(controlsTimeoutRef.current);

        if (playback.isPlaying) {
            controlsTimeoutRef.current = window.setTimeout(() => {
                setShowControls(false);
                setShowSpeedMenu(false);
            }, 3000);
        }
    }, [playback.isPlaying]);

    // Video event handlers
    const handleLoadedMetadata = useCallback(() => {
        if (videoRef.current) {
            setPlayback(prev => ({
                ...prev,
                duration: videoRef.current!.duration,
            }));
            setIsVideoReady(true);
        }
    }, []);

    const handleTimeUpdate = useCallback(() => {
        if (videoRef.current) {
            setPlayback(prev => ({
                ...prev,
                currentTime: videoRef.current!.currentTime,
            }));
        }
    }, []);

    const handleEnded = useCallback(() => {
        setPlayback(prev => ({ ...prev, isPlaying: false }));
        setShowControls(true);
    }, []);

    const handleProgress = useCallback(() => {
        if (videoRef.current && videoRef.current.buffered.length > 0) {
            const bufferedEnd = videoRef.current.buffered.end(videoRef.current.buffered.length - 1);
            const duration = videoRef.current.duration;
            setPlayback(prev => ({
                ...prev,
                bufferedPercent: duration > 0 ? (bufferedEnd / duration) * 100 : 0,
            }));
        }
    }, []);

    const handleError = useCallback(() => {
        onError?.('Failed to load video file');
    }, [onError]);

    // Playback controls
    const togglePlay = useCallback(async () => {
        if (!videoRef.current) return;

        // Use actual video element state, not our playback state
        if (videoRef.current.paused) {
            await videoRef.current.play();
        } else {
            videoRef.current.pause();
        }
        // Note: isPlaying state is updated by onPlay/onPause handlers
        resetControlsTimeout();
    }, [resetControlsTimeout]);

    const seek = useCallback((time: number) => {
        if (videoRef.current) {
            videoRef.current.currentTime = time;
            setPlayback(prev => ({ ...prev, currentTime: time }));
        }
    }, []);

    const setVolume = useCallback((volume: number) => {
        if (videoRef.current) {
            videoRef.current.volume = volume;
            setPlayback(prev => ({ ...prev, volume, isMuted: volume === 0 }));
        }
    }, []);

    const toggleMute = useCallback(() => {
        if (videoRef.current) {
            videoRef.current.muted = !playback.isMuted;
            setPlayback(prev => ({ ...prev, isMuted: !prev.isMuted }));
        }
    }, [playback.isMuted]);

    const setPlaybackRate = useCallback((rate: number) => {
        if (videoRef.current) {
            videoRef.current.playbackRate = rate;
            setPlayback(prev => ({ ...prev, playbackRate: rate }));
            setShowSpeedMenu(false);
        }
    }, []);

    const skipBackward = useCallback(() => {
        seek(Math.max(0, playback.currentTime - 10));
    }, [playback.currentTime, seek]);

    const skipForward = useCallback(() => {
        seek(Math.min(playback.duration, playback.currentTime + 10));
    }, [playback.currentTime, playback.duration, seek]);

    // Fullscreen control
    const toggleFullscreen = useCallback(async () => {
        if (!containerRef.current) return;

        try {
            if (!isFullscreen) {
                if (containerRef.current.requestFullscreen) {
                    await containerRef.current.requestFullscreen();
                }
            } else {
                if (document.exitFullscreen) {
                    await document.exitFullscreen();
                }
            }
        } catch (error) {
            console.error('Fullscreen error:', error);
        }
    }, [isFullscreen]);

    // Picture-in-Picture control
    const togglePiP = useCallback(async () => {
        if (!videoRef.current) return;

        try {
            if (!isPiP) {
                if (document.pictureInPictureEnabled && videoRef.current.requestPictureInPicture) {
                    await videoRef.current.requestPictureInPicture();
                    setIsPiP(true);
                }
            } else {
                if (document.exitPictureInPicture) {
                    await document.exitPictureInPicture();
                    setIsPiP(false);
                }
            }
        } catch (error) {
            console.error('PiP error:', error);
        }
    }, [isPiP]);

    // Listen for fullscreen changes
    useEffect(() => {
        const handleFullscreenChange = () => {
            setIsFullscreen(!!document.fullscreenElement);
        };

        document.addEventListener('fullscreenchange', handleFullscreenChange);
        return () => document.removeEventListener('fullscreenchange', handleFullscreenChange);
    }, []);

    // Listen for PiP changes
    useEffect(() => {
        const video = videoRef.current;
        if (!video) return;

        const handleEnterPiP = () => setIsPiP(true);
        const handleLeavePiP = () => setIsPiP(false);

        video.addEventListener('enterpictureinpicture', handleEnterPiP);
        video.addEventListener('leavepictureinpicture', handleLeavePiP);

        return () => {
            video.removeEventListener('enterpictureinpicture', handleEnterPiP);
            video.removeEventListener('leavepictureinpicture', handleLeavePiP);
        };
    }, []);

    // Keyboard shortcuts
    useEffect(() => {
        const handleKeyDown = (e: KeyboardEvent) => {
            // Only handle if video is focused/active
            if (e.target !== containerRef.current && !containerRef.current?.contains(e.target as Node)) {
                return;
            }

            switch (e.key.toLowerCase()) {
                case ' ':
                    e.preventDefault();
                    togglePlay();
                    break;
                case 'f':
                    e.preventDefault();
                    toggleFullscreen();
                    break;
                case 'm':
                    e.preventDefault();
                    toggleMute();
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
            }
        };

        document.addEventListener('keydown', handleKeyDown);
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [togglePlay, toggleFullscreen, toggleMute, skipBackward, skipForward, setVolume, playback.volume]);

    // Speed options
    const speedOptions = [0.5, 0.75, 1, 1.25, 1.5, 2];

    // Render loading state
    if (!videoSrc) {
        return (
            <div className={`flex items-center justify-center h-full bg-gray-900 ${className}`}>
                <div className="text-gray-500">No video data available</div>
            </div>
        );
    }

    return (
        <div
            ref={containerRef}
            className={`relative flex items-center justify-center h-full bg-black ${className}`}
            onMouseMove={resetControlsTimeout}
            onMouseLeave={() => playback.isPlaying && setShowControls(false)}
            onClick={(e) => {
                // Toggle play on video click (not on controls)
                if (e.target === videoRef.current) {
                    togglePlay();
                }
            }}
            tabIndex={0}
        >
            {/* Video element */}
            <video
                ref={videoRef}
                src={videoSrc}
                className="max-w-full max-h-full"
                onLoadedMetadata={handleLoadedMetadata}
                onTimeUpdate={handleTimeUpdate}
                onEnded={handleEnded}
                onProgress={handleProgress}
                onError={handleError}
                onPlay={() => setPlayback(prev => ({ ...prev, isPlaying: true }))}
                onPause={() => setPlayback(prev => ({ ...prev, isPlaying: false }))}
                playsInline
            />

            {/* Play button overlay (when paused) */}
            {!playback.isPlaying && isVideoReady && (
                <button
                    onClick={togglePlay}
                    className="absolute inset-0 flex items-center justify-center bg-black/30 transition-opacity"
                >
                    <div className="w-20 h-20 flex items-center justify-center bg-white/20 hover:bg-white/30 backdrop-blur-sm rounded-full transition-colors">
                        <Play size={40} className="text-white ml-1" />
                    </div>
                </button>
            )}

            {/* Controls overlay */}
            <div
                className={`absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/90 via-black/50 to-transparent transition-opacity duration-300 ${showControls ? 'opacity-100' : 'opacity-0 pointer-events-none'
                    }`}
            >
                {/* Progress bar */}
                <div className="px-4 pt-8">
                    <div
                        className="relative h-1.5 bg-gray-700 rounded-full cursor-pointer group"
                        onClick={(e) => {
                            const rect = e.currentTarget.getBoundingClientRect();
                            const percent = (e.clientX - rect.left) / rect.width;
                            seek(percent * playback.duration);
                        }}
                    >
                        {/* Buffered */}
                        <div
                            className="absolute h-full bg-gray-500 rounded-full"
                            style={{ width: `${playback.bufferedPercent}%` }}
                        />
                        {/* Progress */}
                        <div
                            className="absolute h-full bg-gradient-to-r from-cyan-400 to-blue-500 rounded-full"
                            style={{ width: `${(playback.currentTime / playback.duration) * 100}%` }}
                        />
                        {/* Thumb */}
                        <div
                            className="absolute top-1/2 -translate-y-1/2 w-4 h-4 bg-white rounded-full shadow-lg opacity-0 group-hover:opacity-100 transition-opacity"
                            style={{ left: `calc(${(playback.currentTime / playback.duration) * 100}% - 8px)` }}
                        />
                    </div>
                </div>

                {/* Control buttons */}
                <div className="flex items-center justify-between px-4 py-3">
                    {/* Left controls */}
                    <div className="flex items-center gap-2">
                        {/* Skip backward */}
                        <button
                            onClick={skipBackward}
                            className="p-2 hover:bg-white/20 rounded-full transition-colors"
                            title="Skip -10s (←)"
                        >
                            <SkipBack size={20} className="text-white" />
                        </button>

                        {/* Play/Pause */}
                        <button
                            onClick={togglePlay}
                            className="p-2 hover:bg-white/20 rounded-full transition-colors"
                            title={playback.isPlaying ? 'Pause (Space)' : 'Play (Space)'}
                        >
                            {playback.isPlaying ? (
                                <Pause size={24} className="text-white" />
                            ) : (
                                <Play size={24} className="text-white ml-0.5" />
                            )}
                        </button>

                        {/* Skip forward */}
                        <button
                            onClick={skipForward}
                            className="p-2 hover:bg-white/20 rounded-full transition-colors"
                            title="Skip +10s (→)"
                        >
                            <SkipForward size={20} className="text-white" />
                        </button>

                        {/* Volume */}
                        <div className="flex items-center gap-1 ml-2">
                            <button
                                onClick={toggleMute}
                                className="p-2 hover:bg-white/20 rounded-full transition-colors"
                                title={playback.isMuted ? 'Unmute (M)' : 'Mute (M)'}
                            >
                                {playback.isMuted || playback.volume === 0 ? (
                                    <VolumeX size={20} className="text-white" />
                                ) : (
                                    <Volume2 size={20} className="text-white" />
                                )}
                            </button>
                            <input
                                type="range"
                                min="0"
                                max="1"
                                step="0.01"
                                value={playback.isMuted ? 0 : playback.volume}
                                onChange={(e) => setVolume(parseFloat(e.target.value))}
                                className="w-20 h-1 appearance-none bg-gray-500 rounded-full cursor-pointer"
                            />
                        </div>

                        {/* Time display */}
                        <span className="text-sm text-white/80 font-mono ml-3">
                            {formatDuration(playback.currentTime)} / {formatDuration(playback.duration)}
                        </span>
                    </div>

                    {/* Right controls */}
                    <div className="flex items-center gap-1">
                        {/* Playback speed */}
                        <div className="relative">
                            <button
                                onClick={() => setShowSpeedMenu(!showSpeedMenu)}
                                className="flex items-center gap-1 px-2 py-1 hover:bg-white/20 rounded transition-colors"
                                title="Playback Speed"
                            >
                                <Gauge size={18} className="text-white" />
                                <span className="text-sm text-white font-mono">{playback.playbackRate}x</span>
                            </button>

                            {showSpeedMenu && (
                                <div className="absolute bottom-full right-0 mb-2 bg-gray-900 border border-gray-700 rounded-lg shadow-xl py-1">
                                    {speedOptions.map((speed) => (
                                        <button
                                            key={speed}
                                            onClick={() => setPlaybackRate(speed)}
                                            className={`w-full px-4 py-1.5 text-left text-sm hover:bg-gray-700 transition-colors ${playback.playbackRate === speed ? 'text-cyan-400' : 'text-white'
                                                }`}
                                        >
                                            {speed}x
                                        </button>
                                    ))}
                                </div>
                            )}
                        </div>

                        {/* Picture-in-Picture */}
                        {document.pictureInPictureEnabled && (
                            <button
                                onClick={togglePiP}
                                className={`p-2 hover:bg-white/20 rounded-full transition-colors ${isPiP ? 'text-cyan-400' : 'text-white'}`}
                                title="Picture-in-Picture"
                            >
                                <PictureInPicture2 size={20} />
                            </button>
                        )}

                        {/* Fullscreen */}
                        <button
                            onClick={toggleFullscreen}
                            className="p-2 hover:bg-white/20 rounded-full transition-colors"
                            title={isFullscreen ? 'Exit Fullscreen (F)' : 'Fullscreen (F)'}
                        >
                            {isFullscreen ? (
                                <Minimize size={20} className="text-white" />
                            ) : (
                                <Maximize size={20} className="text-white" />
                            )}
                        </button>
                    </div>
                </div>
            </div>

            {/* AeroPlayer branding */}
            <div className={`absolute top-3 right-3 text-xs text-white/30 font-mono transition-opacity ${showControls ? 'opacity-100' : 'opacity-0'}`}>
                AeroPlayer
            </div>
        </div>
    );
};

export default VideoPlayer;
