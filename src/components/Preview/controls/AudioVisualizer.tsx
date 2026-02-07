/**
 * Audio Visualizer Component (Canvas-based) - CYBER ENHANCED
 *
 * Creates real-time audio visualization using Web Audio API AnalyserNode.
 *
 * Features:
 * - Multiple visualization modes (bars, waveform, radial, spectrum, fractal, vortex, plasma, kaleidoscope)
 * - Beat detection with onset energy threshold algorithm
 * - Beat-reactive flash/pulse effects across all modes
 * - Cyber Mode with CRT scanlines and glitch effects
 * - Particle system following audio peaks
 * - Tokyo Night / Cyberpunk color theme
 */

import React, { useRef, useEffect, useCallback } from 'react';

export type VisualizerMode = 'bars' | 'waveform' | 'radial' | 'spectrum' | 'fractal' | 'vortex' | 'plasma' | 'kaleidoscope';

interface Particle {
    x: number;
    y: number;
    vx: number;
    vy: number;
    life: number;
    maxLife: number;
    size: number;
    color: string;
}

interface AudioVisualizerProps {
    analyser: AnalyserNode | null;
    mode?: VisualizerMode;
    isPlaying: boolean;
    cyberMode?: boolean;
    className?: string;
}

// Tokyo Night / Cyberpunk color palette
const COLORS = {
    primary: '#7aa2f7',    // Blue
    secondary: '#bb9af7',  // Purple
    accent: '#7dcfff',     // Cyan
    hot: '#f7768e',        // Pink/Red
    background: '#1a1b26', // Dark
    glow: 'rgba(122, 162, 247, 0.4)',
    scanline: 'rgba(0, 255, 255, 0.03)',
};

// Beat detection constants
const ENERGY_HISTORY_SIZE = 43; // ~720ms at 60fps
const BEAT_THRESHOLD = 1.5;     // Current energy must be 1.5x average
const BEAT_MIN_ENERGY = 0.15;   // Minimum absolute energy to count as beat
const BEAT_COOLDOWN_MS = 100;   // Minimum ms between beats

export const AudioVisualizer: React.FC<AudioVisualizerProps> = ({
    analyser,
    mode = 'bars',
    isPlaying,
    cyberMode = false,
    className = '',
}) => {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const animationRef = useRef<number>(0);
    const dataArrayRef = useRef<Uint8Array<ArrayBuffer> | null>(null);
    const particlesRef = useRef<Particle[]>([]);
    const frameCountRef = useRef(0);
    const glitchRef = useRef({ active: false, offset: 0, duration: 0 });

    // Beat detection state
    const energyHistoryRef = useRef<Float32Array>(new Float32Array(ENERGY_HISTORY_SIZE));
    const energyIndexRef = useRef(0);
    const energyCountRef = useRef(0);
    const lastBeatTimeRef = useRef(0);
    const isBeatRef = useRef(false);
    const beatDecayRef = useRef(0); // 1.0 at beat, decays to 0

    // Initialize data array when analyser changes
    useEffect(() => {
        if (analyser) {
            const bufferLength = analyser.frequencyBinCount;
            const buffer = new ArrayBuffer(bufferLength);
            dataArrayRef.current = new Uint8Array(buffer);
        }
    }, [analyser]);

    // Create particles on beat
    const createParticle = useCallback((x: number, y: number, intensity: number) => {
        if (particlesRef.current.length >= 200) return;
        const colors = [COLORS.primary, COLORS.secondary, COLORS.accent, COLORS.hot];
        particlesRef.current.push({
            x,
            y,
            vx: (Math.random() - 0.5) * 4 * intensity,
            vy: -Math.random() * 3 * intensity - 1,
            life: 1,
            maxLife: 30 + Math.random() * 30,
            size: 2 + Math.random() * 3 * intensity,
            color: colors[Math.floor(Math.random() * colors.length)],
        });
    }, []);

    // Update and draw particles
    const updateParticles = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number) => {
        particlesRef.current = particlesRef.current.filter(p => {
            p.x += p.vx;
            p.y += p.vy;
            p.vy += 0.05; // gravity
            p.life -= 1 / p.maxLife;

            if (p.life <= 0) return false;

            ctx.globalAlpha = p.life;
            ctx.shadowBlur = 10;
            ctx.shadowColor = p.color;
            ctx.fillStyle = p.color;
            ctx.beginPath();
            ctx.arc(p.x, p.y, p.size * p.life, 0, Math.PI * 2);
            ctx.fill();
            ctx.globalAlpha = 1;
            ctx.shadowBlur = 0;

            return true;
        });
    }, []);

    // Draw CRT scanlines
    const drawScanlines = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number) => {
        ctx.fillStyle = COLORS.scanline;
        for (let y = 0; y < height; y += 3) {
            ctx.fillRect(0, y, width, 1);
        }
    }, []);

    // Draw glitch effect
    const drawGlitch = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number) => {
        const glitch = glitchRef.current;

        // Random glitch trigger
        if (Math.random() < 0.02 && !glitch.active) {
            glitch.active = true;
            glitch.offset = (Math.random() - 0.5) * 20;
            glitch.duration = 3 + Math.floor(Math.random() * 5);
        }

        if (glitch.active) {
            // RGB shift effect
            const shiftAmount = Math.floor(Math.abs(glitch.offset));

            // Draw with color separation
            ctx.fillStyle = `rgba(255, 0, 0, 0.1)`;
            ctx.fillRect(shiftAmount, 0, width, height);
            ctx.fillStyle = `rgba(0, 255, 255, 0.1)`;
            ctx.fillRect(-shiftAmount, 0, width, height);

            glitch.duration--;
            if (glitch.duration <= 0) {
                glitch.active = false;
            }
        }
    }, []);

    // Draw vignette overlay (darkened edges)
    const drawVignette = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number, intensity: number) => {
        const gradient = ctx.createRadialGradient(
            width / 2, height / 2, Math.min(width, height) * 0.3,
            width / 2, height / 2, Math.max(width, height) * 0.7
        );
        gradient.addColorStop(0, 'transparent');
        gradient.addColorStop(1, `rgba(0, 0, 0, ${0.4 + intensity * 0.3})`);
        ctx.fillStyle = gradient;
        ctx.fillRect(0, 0, width, height);
    }, []);

    // Draw chromatic aberration (RGB channel split)
    const drawChromaticAberration = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number, intensity: number) => {
        if (intensity < 0.3) return;
        const offset = Math.floor(intensity * 4);
        if (offset < 1) return;
        const imageData = ctx.getImageData(0, 0, width * window.devicePixelRatio, height * window.devicePixelRatio);
        const data = imageData.data;
        const w = imageData.width;
        const shiftPixels = offset * window.devicePixelRatio;
        // Shift red channel left, blue channel right
        for (let y = 0; y < imageData.height; y++) {
            for (let x = shiftPixels; x < w - shiftPixels; x++) {
                const i = (y * w + x) * 4;
                // Red from left
                data[i] = data[(y * w + (x - shiftPixels)) * 4];
                // Blue from right
                data[i + 2] = data[(y * w + (x + shiftPixels)) * 4 + 2];
            }
        }
        ctx.putImageData(imageData, 0, 0);
    }, []);

    // Beat detection — returns true on onset
    const detectBeat = useCallback((dataArray: Uint8Array): boolean => {
        const bassEnd = Math.min(16, dataArray.length);
        let bassSum = 0;
        for (let i = 0; i < bassEnd; i++) {
            bassSum += dataArray[i] / 255;
        }
        const currentEnergy = bassSum / bassEnd;

        // Circular buffer write (O(1))
        const history = energyHistoryRef.current;
        const idx = energyIndexRef.current;
        history[idx] = currentEnergy;
        energyIndexRef.current = (idx + 1) % ENERGY_HISTORY_SIZE;
        if (energyCountRef.current < ENERGY_HISTORY_SIZE) {
            energyCountRef.current++;
        }

        // Need enough history
        if (energyCountRef.current < 8) return false;

        // Average over filled portion
        let sum = 0;
        for (let i = 0; i < energyCountRef.current; i++) {
            sum += history[i];
        }
        const avgEnergy = sum / energyCountRef.current;

        const now = performance.now();
        const cooldownOk = (now - lastBeatTimeRef.current) > BEAT_COOLDOWN_MS;

        if (currentEnergy > avgEnergy * BEAT_THRESHOLD && currentEnergy > BEAT_MIN_ENERGY && cooldownOk) {
            lastBeatTimeRef.current = now;
            return true;
        }
        return false;
    }, []);

    // Draw beat flash overlay
    const drawBeatFlash = useCallback((ctx: CanvasRenderingContext2D, width: number, height: number, decay: number) => {
        if (decay <= 0) return;
        const alpha = decay * 0.12;
        ctx.fillStyle = cyberMode
            ? `rgba(0, 255, 255, ${alpha})`    // Cyan flash in cyber mode
            : `rgba(122, 162, 247, ${alpha})`;  // Blue flash normally
        ctx.fillRect(0, 0, width, height);
    }, [cyberMode]);

    // Draw frequency bars
    const drawBars = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        beatDecay: number
    ) => {
        const barCount = 64;
        const barWidth = (width / barCount) - 2;
        const step = Math.floor(dataArray.length / barCount);

        for (let i = 0; i < barCount; i++) {
            const value = dataArray[i * step];
            const percent = value / 255;
            const barHeight = height * percent * 0.9;
            const x = i * (barWidth + 2);
            const y = height - barHeight;

            // Create gradient for each bar
            const gradient = ctx.createLinearGradient(x, height, x, y);
            gradient.addColorStop(0, COLORS.primary);
            gradient.addColorStop(0.5, COLORS.secondary);
            gradient.addColorStop(1, COLORS.accent);

            // Dynamic glow based on bass + beat
            ctx.shadowBlur = 10 + bassIntensity * 20 + beatDecay * 15;
            ctx.shadowColor = beatDecay > 0.5 ? COLORS.hot : COLORS.glow;

            // Draw bar with rounded top
            ctx.fillStyle = gradient;
            ctx.beginPath();
            ctx.roundRect(x, y, barWidth, barHeight, [barWidth / 2, barWidth / 2, 0, 0]);
            ctx.fill();

            // Beat flash on bar tops
            if (beatDecay > 0.3 && percent > 0.5) {
                ctx.fillStyle = `rgba(247, 118, 142, ${beatDecay * 0.6})`;
                ctx.fillRect(x, y, barWidth, 3);
            }

            // Spawn particles on high peaks (more during beats)
            const particleChance = beatDecay > 0.5 ? 0.5 : 0.3;
            if (cyberMode && percent > 0.8 && Math.random() < particleChance) {
                createParticle(x + barWidth / 2, y, percent);
            }

            ctx.shadowBlur = 0;
        }
    }, [cyberMode, createParticle]);

    // Draw radial visualizer (circular)
    const drawRadial = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        beatDecay: number
    ) => {
        const centerX = width / 2;
        const centerY = height / 2;
        const baseRadius = Math.min(width, height) * 0.25;
        const barCount = 64;
        const angleStep = (Math.PI * 2) / barCount;

        ctx.save();
        ctx.translate(centerX, centerY);

        // Beat-reactive radius expansion
        const beatExpand = 1 + beatDecay * 0.15;

        for (let i = 0; i < barCount; i++) {
            const value = dataArray[Math.floor(i * dataArray.length / barCount)];
            const percent = value / 255;
            const barLength = baseRadius * percent * 0.8 * beatExpand;
            const angle = i * angleStep - Math.PI / 2;

            const innerRadius = baseRadius * (0.5 + bassIntensity * 0.2);
            const outerRadius = innerRadius + barLength;

            const x1 = Math.cos(angle) * innerRadius;
            const y1 = Math.sin(angle) * innerRadius;
            const x2 = Math.cos(angle) * outerRadius;
            const y2 = Math.sin(angle) * outerRadius;

            // Gradient along the bar
            const gradient = ctx.createLinearGradient(x1, y1, x2, y2);
            gradient.addColorStop(0, COLORS.primary);
            gradient.addColorStop(1, percent > 0.7 ? COLORS.hot : COLORS.accent);

            ctx.shadowBlur = 8 + bassIntensity * 15 + beatDecay * 10;
            ctx.shadowColor = beatDecay > 0.5 ? COLORS.hot : COLORS.glow;
            ctx.strokeStyle = gradient;
            ctx.lineWidth = 3;
            ctx.beginPath();
            ctx.moveTo(x1, y1);
            ctx.lineTo(x2, y2);
            ctx.stroke();

            // Particles on peaks
            if (cyberMode && percent > 0.85 && Math.random() < 0.2) {
                createParticle(centerX + x2, centerY + y2, percent);
            }
        }

        ctx.restore();
        ctx.shadowBlur = 0;
    }, [cyberMode, createParticle]);

    // Draw spectrum analyzer (horizontal)
    const drawSpectrum = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        beatDecay: number
    ) => {
        // Draw filled area under the curve
        ctx.beginPath();
        ctx.moveTo(0, height);

        const points: { x: number; y: number }[] = [];
        for (let i = 0; i < dataArray.length; i++) {
            const x = (i / dataArray.length) * width;
            const value = dataArray[i] / 255;
            const y = height - value * height * 0.85;
            points.push({ x, y });
        }

        // Smooth curve through points
        for (let i = 0; i < points.length; i++) {
            if (i === 0) {
                ctx.moveTo(points[i].x, points[i].y);
            } else {
                const xc = (points[i].x + points[i - 1].x) / 2;
                const yc = (points[i].y + points[i - 1].y) / 2;
                ctx.quadraticCurveTo(points[i - 1].x, points[i - 1].y, xc, yc);
            }
        }

        ctx.lineTo(width, height);
        ctx.closePath();

        // Gradient fill — shifts warmer on beat
        const gradient = ctx.createLinearGradient(0, 0, 0, height);
        if (beatDecay > 0.3) {
            gradient.addColorStop(0, `${COLORS.hot}90`);
            gradient.addColorStop(0.5, `${COLORS.secondary}60`);
            gradient.addColorStop(1, `${COLORS.primary}20`);
        } else {
            gradient.addColorStop(0, `${COLORS.accent}90`);
            gradient.addColorStop(0.5, `${COLORS.secondary}60`);
            gradient.addColorStop(1, `${COLORS.primary}20`);
        }

        ctx.fillStyle = gradient;
        ctx.shadowBlur = 15 + bassIntensity * 20 + beatDecay * 10;
        ctx.shadowColor = COLORS.accent;
        ctx.fill();

        // Draw line on top
        ctx.strokeStyle = beatDecay > 0.3 ? COLORS.hot : COLORS.accent;
        ctx.lineWidth = 2 + beatDecay;
        ctx.beginPath();
        for (let i = 0; i < points.length; i++) {
            if (i === 0) {
                ctx.moveTo(points[i].x, points[i].y);
            } else {
                const xc = (points[i].x + points[i - 1].x) / 2;
                const yc = (points[i].y + points[i - 1].y) / 2;
                ctx.quadraticCurveTo(points[i - 1].x, points[i - 1].y, xc, yc);
            }
        }
        ctx.stroke();
        ctx.shadowBlur = 0;
    }, []);

    // Draw waveform oscilloscope
    const drawWaveform = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        beatDecay: number
    ) => {
        const sliceWidth = width / dataArray.length;
        let x = 0;

        // Glow effect — intensified on beat
        ctx.shadowBlur = 15 + bassIntensity * 25 + beatDecay * 15;
        ctx.shadowColor = beatDecay > 0.5 ? COLORS.hot : COLORS.accent;

        // Draw waveform line
        ctx.lineWidth = 2 + bassIntensity * 2 + beatDecay * 1.5;
        ctx.strokeStyle = beatDecay > 0.3 ? COLORS.hot : COLORS.accent;
        ctx.beginPath();

        for (let i = 0; i < dataArray.length; i++) {
            const v = dataArray[i] / 128.0;
            const y = (v * height) / 2;

            if (i === 0) {
                ctx.moveTo(x, y);
            } else {
                ctx.lineTo(x, y);
            }

            x += sliceWidth;
        }

        ctx.lineTo(width, height / 2);
        ctx.stroke();

        // Second pass with different color for depth
        ctx.strokeStyle = `${COLORS.secondary}40`;
        ctx.lineWidth = 4 + bassIntensity * 3;
        ctx.stroke();

        ctx.shadowBlur = 0;
    }, []);

    // Draw Lissajous fractal curves
    const drawFractal = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        frame: number,
        beatDecay: number
    ) => {
        const centerX = width / 2;
        const centerY = height / 2;
        const time = frame * 0.02;

        // Get audio-reactive parameters
        const bass = dataArray.slice(0, 8).reduce((a, b) => a + b, 0) / 8 / 255;
        const mid = dataArray.slice(8, 64).reduce((a, b) => a + b, 0) / 56 / 255;
        const high = dataArray.slice(64, 128).reduce((a, b) => a + b, 0) / 64 / 255;

        // Lissajous curve parameters modulated by audio
        const a = 3 + Math.floor(bass * 5);
        const b = 2 + Math.floor(mid * 4);
        const delta = time + high * Math.PI;

        // Beat-reactive scale
        const beatScale = 1 + beatDecay * 0.1;
        const radius = Math.min(width, height) * 0.35 * (0.8 + bassIntensity * 0.4) * beatScale;
        const points = 360;

        // Draw multiple layers for depth
        for (let layer = 0; layer < 3; layer++) {
            const layerOffset = layer * 0.5;
            const alpha = 1 - layer * 0.25;

            ctx.shadowBlur = 15 + bassIntensity * 30 + beatDecay * 15;
            ctx.shadowColor = layer === 0 ? COLORS.accent : layer === 1 ? COLORS.secondary : COLORS.hot;
            ctx.strokeStyle = layer === 0 ? COLORS.primary : layer === 1 ? COLORS.secondary : COLORS.accent;
            ctx.lineWidth = 2 - layer * 0.5;
            ctx.globalAlpha = alpha;

            ctx.beginPath();
            for (let i = 0; i <= points; i++) {
                const t = (i / points) * Math.PI * 2;
                const x = centerX + Math.sin(a * t + delta + layerOffset) * radius;
                const y = centerY + Math.sin(b * t + time * (1 + layer * 0.2)) * radius * 0.8;

                if (i === 0) ctx.moveTo(x, y);
                else ctx.lineTo(x, y);
            }
            ctx.stroke();
        }

        ctx.globalAlpha = 1;
        ctx.shadowBlur = 0;
    }, []);

    // Draw spiral vortex
    const drawVortex = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        frame: number,
        beatDecay: number
    ) => {
        const centerX = width / 2;
        const centerY = height / 2;
        const time = frame * 0.015;

        // Multiple spiral arms
        const arms = 6;
        const maxRadius = Math.min(width, height) * 0.45;

        for (let arm = 0; arm < arms; arm++) {
            const armOffset = (arm / arms) * Math.PI * 2;
            const colorIndex = arm % 4;
            const colors = [COLORS.primary, COLORS.secondary, COLORS.accent, COLORS.hot];

            ctx.shadowBlur = 12 + bassIntensity * 20 + beatDecay * 10;
            ctx.shadowColor = colors[colorIndex];
            ctx.strokeStyle = colors[colorIndex];
            ctx.lineWidth = 2 + bassIntensity * 2;

            ctx.beginPath();

            // Draw spiral
            for (let i = 0; i < 128; i++) {
                const freqValue = dataArray[i] / 255;
                const progress = i / 128;
                const radius = progress * maxRadius * (0.5 + freqValue * 0.5);
                const angle = progress * Math.PI * 4 + armOffset + time;

                const x = centerX + Math.cos(angle) * radius;
                const y = centerY + Math.sin(angle) * radius;

                if (i === 0) ctx.moveTo(x, y);
                else ctx.lineTo(x, y);
            }
            ctx.stroke();
        }

        // Center pulse — bigger on beat
        const pulseRadius = 20 + bassIntensity * 40 + beatDecay * 25;
        const gradient = ctx.createRadialGradient(centerX, centerY, 0, centerX, centerY, pulseRadius);
        gradient.addColorStop(0, beatDecay > 0.5 ? COLORS.accent : COLORS.hot);
        gradient.addColorStop(0.5, `${COLORS.secondary}80`);
        gradient.addColorStop(1, 'transparent');

        ctx.fillStyle = gradient;
        ctx.beginPath();
        ctx.arc(centerX, centerY, pulseRadius, 0, Math.PI * 2);
        ctx.fill();

        ctx.shadowBlur = 0;
    }, []);

    // Draw retro plasma effect
    const drawPlasma = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        frame: number
    ) => {
        const time = frame * 0.03;
        const resolution = 8; // Lower = more detail, higher = faster

        // Audio-reactive color shift
        const bass = dataArray.slice(0, 16).reduce((a, b) => a + b, 0) / 16 / 255;
        const mid = dataArray.slice(16, 64).reduce((a, b) => a + b, 0) / 48 / 255;

        for (let x = 0; x < width; x += resolution) {
            for (let y = 0; y < height; y += resolution) {
                // Classic plasma formula with audio modulation
                const px = x / width - 0.5;
                const py = y / height - 0.5;

                let value = Math.sin(px * 10 + time);
                value += Math.sin(py * 10 + time * 0.5);
                value += Math.sin((px + py) * 10 + time * 0.7);
                value += Math.sin(Math.sqrt(px * px + py * py) * 10 + time + bass * 5);
                value += Math.sin(Math.sqrt((px - 0.5) * (px - 0.5) + py * py) * 8 + mid * 3);

                // Normalize to 0-1 with audio influence
                value = (value / 5 + 1) / 2 + bassIntensity * 0.2;

                // Create color from value
                const r = Math.sin(value * Math.PI * 2) * 127 + 128;
                const g = Math.sin(value * Math.PI * 2 + 2) * 127 + 128;
                const b = Math.sin(value * Math.PI * 2 + 4) * 127 + 128;

                ctx.fillStyle = `rgb(${r}, ${g}, ${b})`;
                ctx.fillRect(x, y, resolution, resolution);
            }
        }

        // Add glow overlay based on audio
        ctx.fillStyle = `rgba(122, 162, 247, ${bassIntensity * 0.2})`;
        ctx.fillRect(0, 0, width, height);
    }, []);

    // Draw kaleidoscope with 8-fold symmetry
    const drawKaleidoscope = useCallback((
        ctx: CanvasRenderingContext2D,
        width: number,
        height: number,
        dataArray: Uint8Array,
        bassIntensity: number,
        frame: number,
        beatDecay: number
    ) => {
        const centerX = width / 2;
        const centerY = height / 2;
        const time = frame * 0.01;
        const segments = 8; // 8-fold symmetry

        // Audio analysis
        const bass = dataArray.slice(0, 16).reduce((a, b) => a + b, 0) / 16 / 255;
        const mid = dataArray.slice(16, 64).reduce((a, b) => a + b, 0) / 48 / 255;
        const high = dataArray.slice(64, 128).reduce((a, b) => a + b, 0) / 64 / 255;

        // Beat-reactive scale
        const beatScale = 1 + beatDecay * 0.08;

        ctx.save();
        ctx.translate(centerX, centerY);
        ctx.scale(beatScale, beatScale);

        // Draw in each segment
        for (let seg = 0; seg < segments; seg++) {
            ctx.save();
            ctx.rotate((seg * Math.PI * 2) / segments);

            // Mirror every other segment
            if (seg % 2 === 1) {
                ctx.scale(-1, 1);
            }

            // Clip to segment
            ctx.beginPath();
            ctx.moveTo(0, 0);
            ctx.lineTo(width, 0);
            ctx.arc(0, 0, width, 0, Math.PI / segments);
            ctx.closePath();
            ctx.clip();

            // Draw audio-reactive shapes
            const maxRadius = Math.min(width, height) * 0.4;

            // Layer 1: Radial lines from audio
            for (let i = 0; i < 32; i++) {
                const value = dataArray[i * 4] / 255;
                const angle = (i / 32) * (Math.PI / segments);
                const radius = value * maxRadius * (0.5 + bass * 0.5);

                const hue = ((time * 50) + i * 10 + high * 100) % 360;
                ctx.strokeStyle = `hsla(${hue}, 80%, 60%, ${0.6 + bassIntensity * 0.4})`;
                ctx.lineWidth = 2 + bass * 3;
                ctx.shadowBlur = 10 + bassIntensity * 20 + beatDecay * 10;
                ctx.shadowColor = `hsla(${hue}, 80%, 60%, 0.8)`;

                ctx.beginPath();
                ctx.moveTo(0, 0);
                ctx.lineTo(Math.cos(angle) * radius, Math.sin(angle) * radius);
                ctx.stroke();
            }

            // Layer 2: Rotating circles
            const numCircles = 5;
            for (let c = 0; c < numCircles; c++) {
                const circleRadius = 10 + c * 15 + mid * 30;
                const orbitRadius = 30 + c * 25 + bass * 40;
                const angle = time * (1 + c * 0.3) + (c * Math.PI / numCircles);

                const cx = Math.cos(angle) * orbitRadius;
                const cy = Math.sin(angle) * orbitRadius * 0.5;

                const hue = ((time * 30) + c * 50) % 360;

                ctx.beginPath();
                ctx.arc(cx, cy, circleRadius, 0, Math.PI * 2);

                const gradient = ctx.createRadialGradient(cx, cy, 0, cx, cy, circleRadius);
                gradient.addColorStop(0, `hsla(${hue}, 90%, 70%, 0.9)`);
                gradient.addColorStop(0.5, `hsla(${(hue + 60) % 360}, 80%, 50%, 0.5)`);
                gradient.addColorStop(1, 'transparent');

                ctx.fillStyle = gradient;
                ctx.fill();
            }

            // Layer 3: Spiral patterns
            ctx.beginPath();
            for (let i = 0; i < 100; i++) {
                const t = i / 100;
                const spiralRadius = t * maxRadius * 0.8 * (0.7 + high * 0.3);
                const spiralAngle = t * Math.PI * 4 + time * 2;
                const waveOffset = Math.sin(t * 10 + time * 3) * 10 * mid;

                const x = Math.cos(spiralAngle) * (spiralRadius + waveOffset);
                const y = Math.sin(spiralAngle) * (spiralRadius + waveOffset) * 0.5;

                if (i === 0) ctx.moveTo(x, y);
                else ctx.lineTo(x, y);
            }

            const hue = (time * 40) % 360;
            ctx.strokeStyle = `hsla(${hue}, 70%, 60%, 0.7)`;
            ctx.lineWidth = 1.5;
            ctx.stroke();

            ctx.restore();
        }

        ctx.restore();
        ctx.shadowBlur = 0;
    }, []);

    // Animation loop
    const draw = useCallback(() => {
        const canvas = canvasRef.current;
        const ctx = canvas?.getContext('2d');

        if (!canvas || !ctx || !analyser || !dataArrayRef.current) {
            animationRef.current = requestAnimationFrame(draw);
            return;
        }

        const width = canvas.width / window.devicePixelRatio;
        const height = canvas.height / window.devicePixelRatio;

        // Clear canvas
        ctx.fillStyle = COLORS.background;
        ctx.fillRect(0, 0, width, height);

        // Get audio data
        analyser.getByteFrequencyData(dataArrayRef.current);

        // Calculate bass intensity (low frequencies)
        const bassRange = Math.floor(dataArrayRef.current.length * 0.1);
        let bassSum = 0;
        for (let i = 0; i < bassRange; i++) {
            bassSum += dataArrayRef.current[i];
        }
        const bassIntensity = (bassSum / bassRange) / 255;

        // Beat detection
        const beatDetected = detectBeat(dataArrayRef.current);
        if (beatDetected) {
            isBeatRef.current = true;
            beatDecayRef.current = 1.0;
        }

        // Decay beat intensity smoothly
        if (beatDecayRef.current > 0) {
            beatDecayRef.current *= 0.92;
            if (beatDecayRef.current < 0.01) beatDecayRef.current = 0;
        }
        const beatDecay = beatDecayRef.current;

        // Draw based on mode
        switch (mode) {
            case 'bars':
                drawBars(ctx, width, height, dataArrayRef.current, bassIntensity, beatDecay);
                break;
            case 'waveform':
                analyser.getByteTimeDomainData(dataArrayRef.current);
                drawWaveform(ctx, width, height, dataArrayRef.current, bassIntensity, beatDecay);
                break;
            case 'radial':
                drawRadial(ctx, width, height, dataArrayRef.current, bassIntensity, beatDecay);
                break;
            case 'spectrum':
                drawSpectrum(ctx, width, height, dataArrayRef.current, bassIntensity, beatDecay);
                break;
            case 'fractal':
                drawFractal(ctx, width, height, dataArrayRef.current, bassIntensity, frameCountRef.current, beatDecay);
                break;
            case 'vortex':
                drawVortex(ctx, width, height, dataArrayRef.current, bassIntensity, frameCountRef.current, beatDecay);
                break;
            case 'plasma':
                drawPlasma(ctx, width, height, dataArrayRef.current, bassIntensity, frameCountRef.current);
                break;
            case 'kaleidoscope':
                drawKaleidoscope(ctx, width, height, dataArrayRef.current, bassIntensity, frameCountRef.current, beatDecay);
                break;
        }

        // Beat flash overlay (subtle white/cyan flash)
        drawBeatFlash(ctx, width, height, beatDecay);

        // Cyber mode effects
        if (cyberMode) {
            // Spawn extra particles on beat
            if (beatDetected) {
                for (let i = 0; i < 5; i++) {
                    createParticle(
                        Math.random() * width,
                        Math.random() * height * 0.5,
                        1.5
                    );
                }
            }

            updateParticles(ctx, width, height);
            drawScanlines(ctx, width, height);

            // Glitch on beat or random
            if (beatDetected || frameCountRef.current % 60 === 0) {
                drawGlitch(ctx, width, height);
            }
        }

        // Post-processing effects
        drawVignette(ctx, width, height, bassIntensity);
        if (cyberMode && beatDecay > 0.4) {
            drawChromaticAberration(ctx, width, height, beatDecay);
        }

        frameCountRef.current++;
        animationRef.current = requestAnimationFrame(draw);
    }, [analyser, mode, cyberMode, detectBeat, drawBeatFlash, drawBars, drawWaveform, drawRadial, drawSpectrum, drawFractal, drawVortex, drawPlasma, drawKaleidoscope, updateParticles, drawScanlines, drawGlitch, createParticle, drawVignette, drawChromaticAberration]);

    // Start/stop animation based on playing state
    useEffect(() => {
        if (isPlaying && analyser) {
            draw();
        } else {
            cancelAnimationFrame(animationRef.current);

            // Draw idle state
            const canvas = canvasRef.current;
            const ctx = canvas?.getContext('2d');
            if (canvas && ctx) {
                const width = canvas.width / window.devicePixelRatio;
                const height = canvas.height / window.devicePixelRatio;

                ctx.fillStyle = COLORS.background;
                ctx.fillRect(0, 0, width, height);

                // Draw center line for waveform mode when idle
                if (mode === 'waveform') {
                    ctx.strokeStyle = `${COLORS.primary}40`;
                    ctx.lineWidth = 1;
                    ctx.beginPath();
                    ctx.moveTo(0, height / 2);
                    ctx.lineTo(width, height / 2);
                    ctx.stroke();
                }

                // Scanlines even when paused if cyber mode is on
                if (cyberMode) {
                    drawScanlines(ctx, width, height);
                }
            }
        }

        return () => cancelAnimationFrame(animationRef.current);
    }, [isPlaying, analyser, mode, cyberMode, draw, drawScanlines]);

    // Handle canvas resize
    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas) return;

        const resizeCanvas = () => {
            const rect = canvas.getBoundingClientRect();
            canvas.width = rect.width * window.devicePixelRatio;
            canvas.height = rect.height * window.devicePixelRatio;
            const ctx = canvas.getContext('2d');
            if (ctx) {
                ctx.scale(window.devicePixelRatio, window.devicePixelRatio);
            }
        };

        resizeCanvas();
        window.addEventListener('resize', resizeCanvas);
        return () => window.removeEventListener('resize', resizeCanvas);
    }, []);

    return (
        <canvas
            ref={canvasRef}
            className={`w-full h-full ${className}`}
            style={{
                background: COLORS.background,
                borderRadius: '8px',
            }}
        />
    );
};

export default AudioVisualizer;
