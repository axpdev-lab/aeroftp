/**
 * WebGL 2 Audio Visualizer Component
 *
 * Renders GLSL fragment shaders driven by real-time audio data from a
 * Web Audio API AnalyserNode. Uses WebGL 2 for GPU-accelerated rendering
 * with logarithmic spectrum mapping and beat detection.
 *
 * Uniforms passed to shaders:
 *   time        - elapsed seconds since start
 *   resolution  - canvas pixel dimensions (vec2)
 *   spectrum    - 64-band logarithmic frequency data (float[64])
 *   zoom_factor - bass-reactive zoom multiplier
 *   beat_energy - bass energy scaled to 0-3 range
 *   bass_energy - average energy of lowest 8 bands
 *   is_beat     - 1 on onset frame, 0 otherwise
 */

import React, { useRef, useEffect, useCallback, useState } from 'react';
import { VERTEX_SHADER, SHADERS, WebGLShaderName } from './shaders';
import { useI18n } from '../../../i18n';

interface WebGLVisualizerProps {
    analyser: AnalyserNode | null;
    shader: WebGLShaderName;
    isPlaying: boolean;
    className?: string;
    onContextLost?: () => void;
}

/** Cap rendering resolution to reduce GPU load on weak drivers (WebKitGTK). */
const MAX_GL_DIMENSION = 640;

/** Compile a single shader stage; throws on failure with diagnostic info. */
function compileShader(
    gl: WebGL2RenderingContext,
    type: number,
    source: string,
): WebGLShader {
    if (gl.isContextLost()) {
        throw new Error('WebGL context lost');
    }
    const typeName = type === gl.VERTEX_SHADER ? 'vertex' : 'fragment';
    const shader = gl.createShader(type);
    if (!shader) {
        throw new Error(`createShader(${typeName}) returned null — WebGL context may be invalid`);
    }
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        const info = gl.getShaderInfoLog(shader);
        gl.deleteShader(shader);
        throw new Error(`${typeName} shader: ${info ?? 'compilation failed'}`);
    }
    return shader;
}

/** Link vertex + fragment shaders into a program. */
function linkProgram(
    gl: WebGL2RenderingContext,
    vs: WebGLShader,
    fs: WebGLShader,
): WebGLProgram {
    const program = gl.createProgram();
    if (!program) {
        throw new Error('createProgram returned null — WebGL context may be invalid');
    }
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
        const info = gl.getProgramInfoLog(program);
        gl.deleteProgram(program);
        throw new Error(`Link: ${info ?? 'program link failed'}`);
    }
    return program;
}

interface UniformLocations {
    time: WebGLUniformLocation | null;
    resolution: WebGLUniformLocation | null;
    spectrum: WebGLUniformLocation | null;
    zoom_factor: WebGLUniformLocation | null;
    beat_energy: WebGLUniformLocation | null;
    bass_energy: WebGLUniformLocation | null;
    is_beat: WebGLUniformLocation | null;
}

function getUniformLocations(
    gl: WebGL2RenderingContext,
    program: WebGLProgram,
): UniformLocations {
    return {
        time: gl.getUniformLocation(program, 'time'),
        resolution: gl.getUniformLocation(program, 'resolution'),
        spectrum: gl.getUniformLocation(program, 'spectrum'),
        zoom_factor: gl.getUniformLocation(program, 'zoom_factor'),
        beat_energy: gl.getUniformLocation(program, 'beat_energy'),
        bass_energy: gl.getUniformLocation(program, 'bass_energy'),
        is_beat: gl.getUniformLocation(program, 'is_beat'),
    };
}

export const WebGLVisualizer: React.FC<WebGLVisualizerProps> = ({
    analyser,
    shader,
    isPlaying,
    className = '',
    onContextLost,
}) => {
    const { t } = useI18n();
    const canvasRef = useRef<HTMLCanvasElement>(null);

    // WebGL object refs (survive across renders, cleaned up on unmount)
    const glRef = useRef<WebGL2RenderingContext | null>(null);
    const programRef = useRef<WebGLProgram | null>(null);
    const vsRef = useRef<WebGLShader | null>(null);
    const fsRef = useRef<WebGLShader | null>(null);
    const vaoRef = useRef<WebGLVertexArrayObject | null>(null);
    const vboRef = useRef<WebGLBuffer | null>(null);
    const uniformsRef = useRef<UniformLocations | null>(null);

    // Audio data refs (pre-allocated, reused every frame)
    const dataArrayRef = useRef<Uint8Array<ArrayBuffer> | null>(null);
    const spectrumRef = useRef<Float32Array>(new Float32Array(64));

    // Animation state refs
    const rafRef = useRef<number>(0);
    const startTimeRef = useRef<number>(0);
    const prevBassEnergyRef = useRef<number>(0);

    // Track initial shader to prevent double compilation on mount
    const initialShaderRef = useRef<WebGLShaderName>(shader);
    const initDoneRef = useRef(false);

    // Error state for UI feedback
    const [error, setError] = useState<string | null>(null);
    const [webgl2Supported, setWebgl2Supported] = useState(true);

    // ── WebGL 2 initialization (runs once on mount) ───────────────────
    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas) return;

        const gl = canvas.getContext('webgl2', {
            antialias: false,
            alpha: false,
            powerPreference: 'high-performance',
            failIfMajorPerformanceCaveat: false,
        });

        if (!gl) {
            setWebgl2Supported(false);
            return;
        }

        glRef.current = gl;

        // Handle context loss — notify parent to fallback to Canvas 2D
        const handleContextLost = (e: Event) => {
            e.preventDefault();
            cancelAnimationFrame(rafRef.current);
            setError('WebGL context lost');
            onContextLost?.();
        };
        const handleContextRestored = () => setError(null);
        canvas.addEventListener('webglcontextlost', handleContextLost);
        canvas.addEventListener('webglcontextrestored', handleContextRestored);

        try {
            // Compile vertex shader
            const vs = compileShader(gl, gl.VERTEX_SHADER, VERTEX_SHADER);
            vsRef.current = vs;

            // Compile fragment shader
            const fs = compileShader(gl, gl.FRAGMENT_SHADER, SHADERS[shader]);
            fsRef.current = fs;

            // Link program
            const program = linkProgram(gl, vs, fs);
            programRef.current = program;
            gl.useProgram(program);

            // Get uniform locations
            uniformsRef.current = getUniformLocations(gl, program);

            // Setup fullscreen quad (triangle strip)
            const vertices = new Float32Array([
                -1, -1,
                 1, -1,
                -1,  1,
                 1,  1,
            ]);

            const vao = gl.createVertexArray();
            gl.bindVertexArray(vao);
            vaoRef.current = vao;

            const vbo = gl.createBuffer();
            gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
            gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STATIC_DRAW);
            vboRef.current = vbo;

            // position attribute at location 0
            gl.enableVertexAttribArray(0);
            gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

            gl.bindVertexArray(null);

            setError(null);
            initDoneRef.current = true;
        } catch (e) {
            const msg = e instanceof Error ? e.message : 'Shader compilation failed';
            setError(msg);
        }

        // Cleanup on unmount
        return () => {
            cancelAnimationFrame(rafRef.current);
            canvas.removeEventListener('webglcontextlost', handleContextLost);
            canvas.removeEventListener('webglcontextrestored', handleContextRestored);

            const currentGl = glRef.current;
            if (!currentGl) return;

            if (programRef.current) currentGl.deleteProgram(programRef.current);
            if (vsRef.current) currentGl.deleteShader(vsRef.current);
            if (fsRef.current) currentGl.deleteShader(fsRef.current);
            if (vaoRef.current) currentGl.deleteVertexArray(vaoRef.current);
            if (vboRef.current) currentGl.deleteBuffer(vboRef.current);

            programRef.current = null;
            vsRef.current = null;
            fsRef.current = null;
            vaoRef.current = null;
            vboRef.current = null;
            uniformsRef.current = null;

            // Release WebGL context
            const loseCtx = currentGl.getExtension('WEBGL_lose_context');
            if (loseCtx) loseCtx.loseContext();
            glRef.current = null;
        };
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []); // Runs once on mount only

    // ── Shader recompilation (when shader prop changes) ───────────────
    useEffect(() => {
        const gl = glRef.current;
        if (!gl || !vsRef.current) return;

        // Skip on first mount — init effect already compiled the initial shader
        if (shader === initialShaderRef.current && initDoneRef.current) {
            initialShaderRef.current = '' as WebGLShaderName; // Allow future recompilation to same shader
            return;
        }

        try {
            // Delete old fragment shader
            if (fsRef.current) {
                if (programRef.current) gl.detachShader(programRef.current, fsRef.current);
                gl.deleteShader(fsRef.current);
                fsRef.current = null;
            }

            // Compile new fragment shader
            const fs = compileShader(gl, gl.FRAGMENT_SHADER, SHADERS[shader]);
            fsRef.current = fs;

            // Delete old program
            if (programRef.current) {
                gl.deleteProgram(programRef.current);
                programRef.current = null;
            }

            // Re-link program with existing vertex shader and new fragment shader
            const program = linkProgram(gl, vsRef.current, fs);
            programRef.current = program;
            gl.useProgram(program);

            // Re-get uniform locations
            uniformsRef.current = getUniformLocations(gl, program);

            setError(null);
        } catch (e) {
            const msg = e instanceof Error ? e.message : 'Shader compilation failed';
            setError(msg);
        }
    }, [shader]);

    // ── Allocate analyser data buffer ─────────────────────────────────
    useEffect(() => {
        if (analyser) {
            dataArrayRef.current = new Uint8Array(analyser.frequencyBinCount);
        }
    }, [analyser]);

    // ── Canvas resize handling ────────────────────────────────────────
    useEffect(() => {
        const canvas = canvasRef.current;
        if (!canvas) return;

        const observer = new ResizeObserver((entries) => {
            for (const entry of entries) {
                const { width, height } = entry.contentRect;
                // Cap resolution to reduce GPU load — WebKitGTK can't handle full DPR
                let pixelWidth = Math.floor(width);
                let pixelHeight = Math.floor(height);

                // Scale down if exceeds max to protect weak GPU drivers
                if (pixelWidth > MAX_GL_DIMENSION || pixelHeight > MAX_GL_DIMENSION) {
                    const scale = MAX_GL_DIMENSION / Math.max(pixelWidth, pixelHeight);
                    pixelWidth = Math.floor(pixelWidth * scale);
                    pixelHeight = Math.floor(pixelHeight * scale);
                }

                canvas.width = pixelWidth;
                canvas.height = pixelHeight;

                const gl = glRef.current;
                if (gl) {
                    gl.viewport(0, 0, pixelWidth, pixelHeight);
                }
            }
        });

        observer.observe(canvas);

        return () => observer.disconnect();
    }, []);

    // ── Render a single frame with given spectrum data ────────────────
    const renderFrame = useCallback((spectrum: Float32Array, elapsed: number) => {
        const gl = glRef.current;
        const program = programRef.current;
        const vao = vaoRef.current;
        const uniforms = uniformsRef.current;
        const canvas = canvasRef.current;

        if (!gl || !program || !vao || !uniforms || !canvas) return;
        if (gl.isContextLost() || canvas.width === 0 || canvas.height === 0) return;

        gl.useProgram(program);

        // Calculate derived audio values
        let bassSum = 0;
        for (let i = 0; i < 8; i++) {
            bassSum += spectrum[i];
        }
        const bassEnergy = bassSum / 8.0;
        const zoomFactor = 1.0 + bassSum * 0.1;
        const beatEnergy = Math.min(bassEnergy * 3.0, 3.0);

        // Simple onset detection
        const isBeat = bassEnergy > 0.4 && bassEnergy > prevBassEnergyRef.current * 1.3;
        prevBassEnergyRef.current = bassEnergy;

        // Set uniforms
        gl.uniform1f(uniforms.time, elapsed);
        gl.uniform2f(uniforms.resolution, canvas.width, canvas.height);
        gl.uniform1fv(uniforms.spectrum, spectrum);
        gl.uniform1f(uniforms.zoom_factor, zoomFactor);
        gl.uniform1f(uniforms.beat_energy, beatEnergy);
        gl.uniform1f(uniforms.bass_energy, bassEnergy);
        gl.uniform1i(uniforms.is_beat, isBeat ? 1 : 0);

        // Draw fullscreen quad
        gl.bindVertexArray(vao);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        gl.bindVertexArray(null);
    }, []);

    // ── Render loop (start/stop based on isPlaying) ───────────────────
    useEffect(() => {
        if (isPlaying && analyser) {
            startTimeRef.current = performance.now();
            prevBassEnergyRef.current = 0;

            const render = () => {
                const dataArray = dataArrayRef.current;
                if (!dataArray || !analyser) return;

                // Get frequency data
                analyser.getByteFrequencyData(dataArray);

                // Map 256 FFT bins to 64 bands (logarithmic)
                const spectrum = spectrumRef.current;
                for (let i = 0; i < 64; i++) {
                    const startBin = Math.floor(Math.pow(i / 64, 2) * dataArray.length);
                    const endBin = Math.floor(Math.pow((i + 1) / 64, 2) * dataArray.length);
                    let sum = 0;
                    const count = Math.max(1, endBin - startBin);
                    for (let j = startBin; j < endBin && j < dataArray.length; j++) {
                        sum += dataArray[j];
                    }
                    spectrum[i] = (sum / count) / 255.0;
                }

                const elapsed = (performance.now() - startTimeRef.current) / 1000.0;
                renderFrame(spectrum, elapsed);

                rafRef.current = requestAnimationFrame(render);
            };

            rafRef.current = requestAnimationFrame(render);
        } else {
            cancelAnimationFrame(rafRef.current);

            // Draw one idle frame with zero spectrum
            const spectrum = spectrumRef.current;
            spectrum.fill(0);
            prevBassEnergyRef.current = 0;
            renderFrame(spectrum, 0);
        }

        return () => cancelAnimationFrame(rafRef.current);
    }, [isPlaying, analyser, renderFrame]);

    // ── WebGL 2 not supported fallback ────────────────────────────────
    if (!webgl2Supported) {
        return (
            <div className={`relative w-full h-full flex items-center justify-center bg-gray-900 rounded-lg ${className}`}>
                <span className="text-gray-500 text-xs font-mono">{t('preview.webgl.notSupported')}</span>
            </div>
        );
    }

    return (
        <div className={`relative w-full h-full ${className}`}>
            <canvas
                ref={canvasRef}
                className="w-full h-full"
                style={{ background: '#1a1b26', borderRadius: '8px' }}
            />
            {error && (
                <div className="absolute inset-0 flex items-center justify-center bg-gray-900/90 rounded-lg">
                    <span className="text-red-400 text-xs font-mono">{error}</span>
                </div>
            )}
            <span className="absolute bottom-1 right-2 text-[9px] font-mono text-cyan-500/40 pointer-events-none">
                GL
            </span>
        </div>
    );
};

export default WebGLVisualizer;
