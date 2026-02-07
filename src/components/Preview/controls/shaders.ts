/**
 * WebGL 2 Shader Sources - Ported from CyberPulse/AVPLAYER
 *
 * 6 fragment shaders + 1 vertex shader for GPU-accelerated audio visualization.
 * Original: OpenGL 3.3 core -> Ported to: GLSL ES 300 es (WebGL 2)
 *
 * Uniforms (shared across all shaders):
 *   - time: float (playback time in seconds)
 *   - resolution: vec2 (canvas width/height in pixels)
 *   - spectrum: float[64] (FFT frequency bins, normalized 0-1)
 *   - zoom_factor: float (camera zoom multiplier)
 *   - beat_energy: float (smoothed beat intensity 0-1)
 *   - bass_energy: float (low-frequency energy)
 *   - is_beat: int (1 on kick/snare transient, 0 otherwise)
 */

export const VERTEX_SHADER = `#version 300 es
precision highp float;

layout (location = 0) in vec2 aPos;

void main() {
    gl_Position = vec4(aPos, 0.0, 1.0);
}
`;

export const SHADERS: Record<string, string> = {
    wave_glitch: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float beat_energy;
uniform float zoom_factor;
uniform int is_beat;

out vec4 FragColor;

// Hash functions for glitch randomness
float hash12(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

vec2 hash22(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 23.32);
    return fract(vec2(p.x * p.y, (p.x + p.y) * p.x));
}

// Smooth noise
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);

    float a = hash12(i);
    float b = hash12(i + vec2(1.0, 0.0));
    float c = hash12(i + vec2(0.0, 1.0));
    float d = hash12(i + vec2(1.0, 1.0));

    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Sinusoidal wave distortion
vec2 wave_distort(vec2 uv, float t) {
    float bass = (spectrum[0] + spectrum[1] + spectrum[2]) / 3.0;
    float mid = (spectrum[20] + spectrum[25]) / 2.0;

    // Multiple wave layers
    uv.x += sin(uv.y * 10.0 + t * 2.0) * 0.02 * bass;
    uv.y += cos(uv.x * 12.0 - t * 1.5) * 0.02 * mid;

    // Circular wave from center
    vec2 center = uv - 0.5;
    float dist = length(center);
    float wave = sin(dist * 20.0 - t * 3.0) * 0.01 * beat_energy;
    uv += normalize(center) * wave;

    return uv;
}

// Generate procedural pattern
vec3 generate_pattern(vec2 uv) {
    // Create base pattern from spectrum
    float pattern = 0.0;
    for (int i = 0; i < 16; i++) {
        float band = spectrum[i * 4];
        float freq = float(i) * 2.0 + 1.0;
        pattern += sin(uv.x * freq * 3.14159 + time) * sin(uv.y * freq * 3.14159 - time * 0.5) * band;
    }

    // Psychedelic colors
    float hue = pattern * 0.5 + time * 0.3 + uv.x * 0.5;
    vec3 col = 0.5 + 0.5 * cos(6.28318 * (hue + vec3(0.0, 0.33, 0.67)));

    return col;
}

// Pixel sorting glitch
vec3 pixel_sort(vec2 uv, float strength) {
    float high_freq = (spectrum[45] + spectrum[50] + spectrum[55]) / 3.0;

    if (high_freq > 0.3 && hash12(floor(uv * resolution / 4.0)) < strength) {
        // Horizontal sort glitch
        uv.x += (hash12(vec2(uv.y * 100.0, time)) - 0.5) * 0.1;
    }

    return generate_pattern(uv);
}

// Data moshing / corruption
vec3 datamosh(vec2 uv, float t) {
    float treble = (spectrum[50] + spectrum[55] + spectrum[60]) / 3.0;

    // Block corruption
    vec2 block_uv = floor(uv * 20.0) / 20.0;
    float corruption = hash12(block_uv + floor(t * 10.0));

    if (corruption < treble * 0.5) {
        // Offset entire block
        vec2 offset = hash22(block_uv) * 0.1 - 0.05;
        uv += offset;
    }

    return generate_pattern(uv);
}

// Scanlines CRT effect
float scanlines(vec2 uv) {
    float line = sin(uv.y * resolution.y * 1.0) * 0.5 + 0.5;
    return line * 0.15 + 0.85;
}

// Digital noise / static
float digital_noise(vec2 uv, float t) {
    float n = hash12(uv * t * 1000.0);
    return n;
}

void main() {
    vec2 uv = gl_FragCoord.xy / resolution;
    vec2 centered_uv = (gl_FragCoord.xy - 0.5 * resolution) / resolution.y;

    // Apply wave distortion
    uv = wave_distort(uv, time);

    // Treble-triggered glitch intensity
    float treble_avg = (spectrum[50] + spectrum[55] + spectrum[60] + spectrum[63]) * 0.25;
    float glitch_intensity = treble_avg * 2.0;

    vec3 color = vec3(0.0);

    // Choose effect based on beat and treble
    if (is_beat == 1 && glitch_intensity > 0.5) {
        // EXTREME GLITCH MODE

        // Chromatic aberration
        float aberration_strength = 0.02 + glitch_intensity * 0.05;
        vec2 offset_r = (uv - 0.5) * aberration_strength;
        vec2 offset_b = -(uv - 0.5) * aberration_strength;

        float r = pixel_sort(uv + offset_r, 0.3).r;
        float g = datamosh(uv, time).g;
        float b = pixel_sort(uv + offset_b, 0.3).b;

        color = vec3(r, g, b);

        // Add digital noise
        float noise_amount = glitch_intensity * 0.3;
        float dnoise = digital_noise(uv, time);
        color = mix(color, vec3(dnoise), noise_amount * 0.5);

    } else if (glitch_intensity > 0.3) {
        // MODERATE GLITCH
        color = datamosh(uv, time);

        // Subtle chromatic aberration
        vec2 offset = (uv - 0.5) * 0.01;
        color.r = pixel_sort(uv + offset, 0.1).r;
        color.b = datamosh(uv - offset, time).b;

    } else {
        // WAVE MODE - psychedelic waves
        color = generate_pattern(uv);

        // Add glow on wave peaks
        float wave_pattern = sin(centered_uv.x * 10.0 + time * 2.0) * cos(centered_uv.y * 8.0 - time);
        color += vec3(0.2, 0.5, 1.0) * wave_pattern * spectrum[30] * 0.5;
    }

    // CRT scanlines
    color *= scanlines(uv);

    // Vignette
    float vignette = 1.0 - length(centered_uv) * 0.6;
    color *= vignette;

    // Horizontal sync glitch on strong beats
    if (is_beat == 1) {
        float sync_glitch = step(0.98, hash12(vec2(uv.y * 50.0, time)));
        color = mix(color, color.bgr, sync_glitch);
    }

    // Bass pump brightness
    float bass_pump = spectrum[0] * 0.5;
    color *= (1.0 + bass_pump);

    // Bloom on bright areas
    float brightness = dot(color, vec3(0.299, 0.587, 0.114));
    if (brightness > 0.8) {
        color += color * (brightness - 0.8) * 2.0;
    }

    // Random color inversion flashes
    float invert_chance = hash12(vec2(time * 10.0, uv.y));
    if (treble_avg > 0.6 && invert_chance < 0.05) {
        color = vec3(1.0) - color;
    }

    FragColor = vec4(color, 1.0);
}
`,
    glitch_vhs: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float zoom_factor;
uniform float beat_energy;
uniform int is_beat;

out vec4 FragColor;

float random(vec2 st) {
    return fract(sin(dot(st.xy, vec2(12.9898, 78.233))) * 43758.5453123);
}

float noise(vec2 st) {
    vec2 i = floor(st);
    vec2 f = fract(st);
    float a = random(i);
    float b = random(i + vec2(1.0, 0.0));
    float c = random(i + vec2(0.0, 1.0));
    float d = random(i + vec2(1.0, 1.0));
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(a, b, u.x) + (c - a) * u.y * (1.0 - u.x) + (d - b) * u.x * u.y;
}

void main() {
    vec2 uv = gl_FragCoord.xy / resolution;
    vec2 originalUV = uv;

    float bass = spectrum[3];
    float mid = spectrum[20];
    float high = spectrum[50];

    // VHS tape effect
    float vhs_y = uv.y * resolution.y;
    float vhs_shift = sin(vhs_y * 0.5 + time * 10.0) * 0.002;

    // Glitch horizontal displacement
    float glitch = 0.0;
    if (is_beat == 1 || bass > 0.5) {
        float glitchLine = floor(uv.y * 20.0 + time * 5.0);
        glitch = (random(vec2(glitchLine, floor(time * 10.0))) - 0.5) * 0.1 * bass;
    }

    // RGB split
    float split = beat_energy * 0.02 + (is_beat == 1 ? 0.03 : 0.0);
    vec2 uvR = uv + vec2(split + glitch + vhs_shift, 0.0);
    vec2 uvG = uv + vec2(glitch + vhs_shift, 0.0);
    vec2 uvB = uv + vec2(-split + glitch + vhs_shift, 0.0);

    // Scan lines
    float scanline = sin(uv.y * resolution.y * 2.0) * 0.1;

    // Noise overlay
    float noiseVal = noise(uv * resolution * 0.5 + time) * 0.1;

    // Create glitchy pattern
    vec3 col = vec3(0.0);

    // Checkerboard pattern modulated by spectrum
    float checker = mod(floor(uvG.x * 20.0 + time * bass * 5.0) + floor(uvG.y * 20.0 + time * mid * 3.0), 2.0);

    // RGB channels with spectrum
    col.r = checker * high + uvR.x * bass + (is_beat == 1 ? 0.5 : 0.0);
    col.g = (1.0 - checker) * mid + uvG.y * high;
    col.b = checker * bass + (1.0 - uvB.y) * mid + beat_energy;

    // Data corruption effect
    float corrupt = step(0.95, random(vec2(floor(uv.y * 100.0), floor(time * 4.0))));
    if (corrupt > 0.5) {
        col = vec3(random(uv + time), random(uv - time), random(uv * time));
    }

    // Apply effects
    col += noiseVal;
    col *= 1.0 - scanline;

    // Vignette
    float vignette = 1.0 - length(originalUV - 0.5) * 0.5;
    col *= vignette;

    // Cyberpunk color grading
    col = pow(col, vec3(0.8));
    col = col * 0.5 + 0.5 * cos(6.28318 * (col + time * 0.1 + vec3(0.0, 0.33, 0.67)));

    // Beat flash
    if (is_beat == 1) {
        col += vec3(0.2, 0.4, 0.6) * (1.0 - abs(sin(time * 20.0)));
    }

    FragColor = vec4(col, 1.0);
}
`,
    fractal_mandelbrot: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float zoom_factor;
uniform float beat_energy;
uniform int is_beat;

out vec4 FragColor;

void main() {
    vec2 uv = (gl_FragCoord.xy - 0.5 * resolution) / resolution.y;
    uv *= zoom_factor * (1.0 + spectrum[4] * 0.8 + beat_energy * 0.5);
    uv = mat2(cos(time*0.1), -sin(time*0.1), sin(time*0.1), cos(time*0.1)) * uv;

    vec2 c = uv + vec2(-0.745 + sin(time*0.05)*0.01, 0.113 + cos(time*0.07)*0.01);
    vec2 z = vec2(0.0);
    float iter = 0.0;
    const float max_iter = 40.0;

    for (float i = 0.0; i < max_iter; i++) {
        z = vec2(z.x*z.x - z.y*z.y, 2.0*z.x*z.y) + c;
        if (dot(z, z) > 4.0) break;
        iter++;
    }

    float smooth_iter = iter - log(log(dot(z,z)) + 1e-6) / log(2.0);
    vec3 col = 0.5 + 0.5 * cos(3.0 + smooth_iter/20.0 + vec3(0.0, 0.6, 1.0) + spectrum[20]*2.0);
    col *= (1.0 + beat_energy * 1.5);

    if (is_beat == 1) {
        col += vec3(0.5, 0.5, 1.0);
    }

    FragColor = vec4(col, 1.0);
}
`,
    raymarch_tunnel: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float zoom_factor;
uniform float beat_energy;
uniform int is_beat;

out vec4 FragColor;

#define MAX_STEPS 32
#define MAX_DIST 50.0
#define SURF_DIST 0.02

// 3D noise for organic deformation
float hash13(vec3 p) {
    p = fract(p * vec3(443.537, 537.247, 247.428));
    p += dot(p, p.yxz + 19.19);
    return fract((p.x + p.y) * p.z);
}

float noise3(vec3 p) {
    vec3 i = floor(p);
    vec3 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);

    return mix(
        mix(mix(hash13(i + vec3(0,0,0)), hash13(i + vec3(1,0,0)), f.x),
            mix(hash13(i + vec3(0,1,0)), hash13(i + vec3(1,1,0)), f.x), f.y),
        mix(mix(hash13(i + vec3(0,0,1)), hash13(i + vec3(1,0,1)), f.x),
            mix(hash13(i + vec3(0,1,1)), hash13(i + vec3(1,1,1)), f.x), f.y), f.z);
}

// Fractal Brownian Motion for complex detail
float fbm(vec3 p) {
    float value = 0.0;
    float amplitude = 0.5;
    for (int i = 0; i < 2; i++) {
        value += amplitude * noise3(p);
        p *= 2.0;
        amplitude *= 0.5;
    }
    return value;
}

float sdSphere(vec3 p, float r) {
    return length(p) - r;
}

float sdTorus(vec3 p, vec2 t) {
    vec2 q = vec2(length(p.xz) - t.x, p.y);
    return length(q) - t.y;
}

// 3D rotation matrices
mat3 rotateX(float a) {
    float c = cos(a), s = sin(a);
    return mat3(1,0,0, 0,c,-s, 0,s,c);
}

mat3 rotateY(float a) {
    float c = cos(a), s = sin(a);
    return mat3(c,0,s, 0,1,0, -s,0,c);
}

mat3 rotateZ(float a) {
    float c = cos(a), s = sin(a);
    return mat3(c,-s,0, s,c,0, 0,0,1);
}

float GetDist(vec3 p) {
    // Bass-driven contraction/expansion
    float bass = (spectrum[0] + spectrum[1] + spectrum[2] + spectrum[3]) * 0.25;
    float bass_expand = 1.0 + bass * 1.5;

    // Mid frequencies rotation
    float mid = (spectrum[15] + spectrum[20] + spectrum[25]) / 3.0;

    // High frequency glitch
    float high = (spectrum[50] + spectrum[55] + spectrum[60]) / 3.0;

    // Apply rotation based on music
    vec3 rp = p;
    rp = rotateY(time * 0.3 + mid * 3.0) * rp;
    rp = rotateZ(sin(time * 0.5) * 0.5 + high * 2.0) * rp;

    // Organic noise deformation
    float noise_deform = fbm(rp * 0.5 + time * 0.2) * 0.8;
    noise_deform += sin(rp.z * 2.0 + time) * 0.3;

    // Tunnel walls with breathing
    float tunnel_radius = 2.0 * bass_expand;
    float tunnel = length(rp.xy) - tunnel_radius - noise_deform;

    // Add beat pulse to tunnel
    if (is_beat == 1) {
        tunnel -= 0.5;
    }

    float d = tunnel;

    // Floating energy spheres
    for (int i = 0; i < 3; i++) {
        vec3 sp = rp;
        float offset = float(i) * 2.5;
        sp.z = mod(sp.z + time * 3.0 + offset, 8.0) - 4.0;

        // Orbit around center
        float angle = time * 0.5 + float(i) * 1.25;
        float orbit_radius = 1.0 + spectrum[i * 10] * 0.5;
        sp.xy -= vec2(cos(angle), sin(angle)) * orbit_radius;

        float sphere_size = 0.2 + spectrum[i * 8] * 0.3 + beat_energy * 0.1;
        float sphere = sdSphere(sp, sphere_size);

        d = min(d, sphere);
    }

    // Torus rings pulsating
    for (int i = 0; i < 2; i++) {
        vec3 tp = rp;
        tp.z = mod(tp.z + time * 2.0 + float(i) * 3.0, 9.0) - 4.5;

        float torus_major = 1.5 + bass * 0.8 + spectrum[i * 15] * 0.4;
        float torus_minor = 0.15 + beat_energy * 0.1;
        float torus = sdTorus(tp, vec2(torus_major, torus_minor));

        d = min(d, torus);
    }

    return d * 0.8;
}

float RayMarch(vec3 ro, vec3 rd) {
    float dO = 0.0;
    for (int i = 0; i < MAX_STEPS; i++) {
        vec3 p = ro + rd * dO;
        float dS = GetDist(p);
        dO += dS;
        if (dO > MAX_DIST || abs(dS) < SURF_DIST) break;
    }
    return dO;
}

vec3 GetNormal(vec3 p) {
    float d = GetDist(p);
    vec2 e = vec2(0.01, 0.0);
    vec3 n = d - vec3(
        GetDist(p - e.xyy),
        GetDist(p - e.yxy),
        GetDist(p - e.yyx)
    );
    return normalize(n);
}

// Volumetric fog/glow
float volumetric(vec3 ro, vec3 rd, float maxDist) {
    float fog = 0.0;
    float t = 0.0;
    for (int i = 0; i < 12; i++) {
        vec3 p = ro + rd * t;
        float density = max(0.0, 1.0 - GetDist(p) * 0.5);
        fog += density * 0.12;
        t += 0.5;
        if (t > maxDist) break;
    }
    return fog;
}

void main() {
    vec2 uv = (gl_FragCoord.xy - 0.5 * resolution) / resolution.y;

    vec3 col = vec3(0.0);

    // Camera movement forward through tunnel
    vec3 ro = vec3(0.0, 0.0, -time * 2.0);
    vec3 rd = normalize(vec3(uv, 1.0));

    // Apply fisheye distortion from bass
    float bass_distort = (spectrum[0] + spectrum[1]) * 0.5;
    float lens_distort = 1.0 + bass_distort * 0.3;
    rd.xy *= lens_distort;
    rd = normalize(rd);

    float d = RayMarch(ro, rd);

    // Volumetric fog
    float fog = volumetric(ro, rd, min(d, 20.0));

    if (d < MAX_DIST) {
        vec3 p = ro + rd * d;
        vec3 n = GetNormal(p);

        // Dynamic lighting
        vec3 lightPos = vec3(sin(time) * 2.0, cos(time * 0.7) * 2.0, ro.z + 5.0);
        vec3 lightDir = normalize(lightPos - p);
        float diff = max(dot(n, lightDir), 0.0);

        // Specular highlight
        vec3 viewDir = -rd;
        vec3 halfDir = normalize(lightDir + viewDir);
        float spec = pow(max(dot(n, halfDir), 0.0), 32.0);

        // Cyberpunk color palette from spectrum
        float bass_hue = (spectrum[0] + spectrum[2]) * 0.5;
        float mid_hue = (spectrum[20] + spectrum[25]) * 0.5;
        float treble_hue = (spectrum[50] + spectrum[55]) * 0.5;

        float hue = p.z * 0.05 + time * 0.15 + bass_hue - treble_hue * 0.5;
        col = 0.5 + 0.5 * cos(6.28318 * (hue + vec3(0.0, 0.33, 0.67)));

        // Lighting contribution
        col *= (diff * 0.7 + 0.3);
        col += vec3(1.0, 0.8, 0.6) * spec * 0.5;

        // Beat energy flash
        col += vec3(0.0, 0.8, 1.0) * beat_energy * 0.6;

        // Distance fog fade
        col = mix(col, vec3(0.0), smoothstep(0.0, MAX_DIST * 0.5, d));

        // Inner glow
        col += vec3(0.2, 0.5, 1.0) * (1.0 - smoothstep(0.0, 2.0, d));
    } else {
        // Background stars/void
        float stars = noise3(rd * 50.0 + time * 0.1);
        stars = pow(stars, 20.0) * 3.0;
        col = vec3(stars) * vec3(0.5, 0.8, 1.0);
    }

    // Add volumetric fog overlay
    vec3 fog_color = vec3(0.1, 0.4, 0.8) + vec3(0.5, 0.3, 0.0) * beat_energy;
    col += fog_color * fog * 0.5;

    // Bass pump effect
    float pump = spectrum[0] * 0.4;
    col *= (1.0 - pump * 0.3) + pump;

    // Chromatic aberration on beat
    if (is_beat == 1) {
        col.r *= 1.2;
        col.b *= 0.9;
    }

    // Vignette
    float vignette = 1.0 - length(uv) * 0.4;
    col *= vignette;

    FragColor = vec4(col, 1.0);
}
`,
    metaball_pulse: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float beat_energy;
uniform float zoom_factor;
uniform int is_beat;

out vec4 FragColor;

// Smooth minimum for organic blending
float smin(float a, float b, float k) {
    float h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * h * k * (1.0 / 6.0);
}

// 2D rotation matrix
mat2 rot(float a) {
    float c = cos(a), s = sin(a);
    return mat2(c, -s, s, c);
}

// Hash function for pseudo-random
float hash(vec2 p) {
    p = fract(p * vec2(123.45, 678.90));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

// Organic blob distance function
float metaball_scene(vec2 p, float t) {
    float dist = 1e10;

    // Breathing animation from bass
    float bass_avg = (spectrum[0] + spectrum[1] + spectrum[2] + spectrum[3]) * 0.25;
    float breath = 0.3 + bass_avg * 0.7;

    // Mid-range pulsation
    float mid_avg = (spectrum[10] + spectrum[15] + spectrum[20]) / 3.0;
    float pulse = 0.2 + mid_avg * 0.5;

    // Kick explosive expansion
    float kick_expand = is_beat == 1 ? 0.4 : 0.0;

    // Central blob - main breathing organism
    vec2 center = vec2(0.0);
    float central_radius = 0.4 * breath + kick_expand;
    float central = length(p - center) - central_radius;

    // Orbiting blobs - 5 satellites
    for (int i = 0; i < 5; i++) {
        float angle = t * 0.3 + float(i) * 6.28 / 5.0;
        float spec_offset = spectrum[i * 3] * 0.3;
        float orbit_radius = 0.6 + spec_offset;
        vec2 pos = vec2(cos(angle), sin(angle)) * orbit_radius;

        // Organic size variation
        float blob_size = 0.15 + spectrum[i * 4] * 0.15 + pulse * 0.1;
        float blob = length(p - pos) - blob_size;

        // Smooth blend with central
        central = smin(central, blob, 0.3);
    }

    // Additional micro blobs for texture
    for (int i = 0; i < 4; i++) {
        float angle = -t * 0.5 + float(i) * 6.28 / 8.0 + spectrum[i * 2] * 2.0;
        float orbit = 0.3 + sin(t + float(i)) * 0.2;
        vec2 pos = vec2(cos(angle), sin(angle)) * orbit;
        float size = 0.08 + spectrum[i * 5] * 0.08;
        float micro = length(p - pos) - size;
        central = smin(central, micro, 0.25);
    }

    return central;
}

// HSV to RGB conversion
vec3 hsv2rgb(vec3 c) {
    vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}

void main() {
    // Normalized coordinates (-1 to 1)
    vec2 uv = (gl_FragCoord.xy - 0.5 * resolution) / resolution.y;

    // Rotation based on mid-high frequencies
    float high_energy = (spectrum[30] + spectrum[40] + spectrum[50]) / 3.0;
    uv = rot(time * 0.2 + high_energy * 2.0) * uv;

    // Calculate distance to metaball field
    float d = metaball_scene(uv, time);

    // Glow effect
    float glow = 0.02 / (abs(d) + 0.01);
    glow = pow(glow, 1.5);

    // Color cycling based on spectrum
    float hue = fract(time * 0.1 + spectrum[20] * 0.5);

    // Bass shifts to cyan/blue, highs shift to magenta/yellow
    float bass_hue = (spectrum[0] + spectrum[1]) * 0.5;
    float treble_hue = (spectrum[50] + spectrum[60]) * 0.5;
    hue += bass_hue * 0.3 - treble_hue * 0.2;

    // Base color from HSV
    vec3 color = hsv2rgb(vec3(hue, 0.8, 1.0));

    // Inside vs outside coloring
    if (d < 0.0) {
        // Inside blob - saturated colors
        float intensity = 0.6 + 0.4 * (1.0 - smoothstep(-0.3, 0.0, d));
        color *= intensity * (0.8 + beat_energy * 0.3);
        color += vec3(0.1, 0.3, 0.5) * beat_energy * 0.5;
    } else {
        // Outside - glow corona
        color *= glow * 0.8;
        color += vec3(0.0, 0.8, 1.0) * glow * 0.25;
    }

    // Kick drum cyan flash
    if (is_beat == 1) {
        color += vec3(0.2, 0.7, 1.0) * 0.4 * exp(-abs(d) * 4.0);
    }

    // Add chromatic aberration on edges
    float edge = smoothstep(0.0, 0.05, abs(d));
    color.r += glow * 0.2 * (1.0 - edge);
    color.b += glow * 0.3 * (1.0 - edge);

    FragColor = vec4(color, 1.0);
}
`,
    particles_explosion: `#version 300 es
precision highp float;

uniform float time;
uniform vec2 resolution;
uniform float spectrum[64];
uniform float beat_energy;
uniform float zoom_factor;
uniform int is_beat;

out vec4 FragColor;

// Hash for pseudo-random
float hash21(vec2 p) {
    p = fract(p * vec2(234.56, 789.12));
    p += dot(p, p + 34.56);
    return fract(p.x * p.y);
}

vec2 hash22(vec2 p) {
    p = fract(p * vec2(123.45, 678.90));
    p += dot(p, p + 45.32);
    return fract(vec2(p.x * p.y, (p.x + p.y) * p.y));
}

// HSV to RGB
vec3 hsv2rgb(vec3 c) {
    vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}

// Particle system
vec3 render_particles(vec2 uv, float t) {
    vec3 color = vec3(0.0);

    // Multiple particle layers with different speeds/colors
    for (int layer = 0; layer < 2; layer++) {
        float layer_time = t * (1.0 + float(layer) * 0.3);
        float layer_speed = 0.5 + float(layer) * 0.25;

        // Number of particles based on spectrum intensity
        int num_particles = 30 + int(beat_energy * 20.0);

        for (int i = 0; i < 40; i++) {
            if (i >= num_particles) break;

            vec2 seed = vec2(float(i) * 0.1, float(layer) * 100.0);
            float birth_time = hash21(seed) * 10.0;
            float local_t = mod(layer_time + birth_time, 5.0);

            // Particle dies and respawns
            if (local_t > 3.0) continue;

            // Random direction
            vec2 rand = hash22(seed);
            float angle = rand.x * 6.28318;
            vec2 dir = vec2(cos(angle), sin(angle));

            // Velocity influenced by spectrum band
            int spec_idx = int(rand.y * 60.0);
            float spec_boost = 1.0 + spectrum[spec_idx] * 2.0;

            // Position expands outward
            vec2 pos = dir * local_t * layer_speed * spec_boost;

            // Distance from particle to pixel
            float dist = length(uv - pos);

            // Particle size decreases with age
            float size = 0.05 * (1.0 - local_t / 3.0);
            size += beat_energy * 0.02;

            // Glow intensity
            float glow = size / (dist + 0.01);
            glow = pow(glow, 2.0) * (1.0 - local_t / 3.0);

            // Color based on direction and spectrum
            float hue = angle / 6.28318 + spectrum[spec_idx] * 0.3 + t * 0.1;
            vec3 particle_color = hsv2rgb(vec3(hue, 0.9, 1.0));

            // Layer-specific color tint
            if (layer == 0) particle_color *= vec3(0.2, 1.0, 1.0); // Cyan
            if (layer == 1) particle_color *= vec3(1.0, 0.3, 1.0); // Magenta
            if (layer == 2) particle_color *= vec3(0.3, 1.0, 0.3); // Green

            color += particle_color * glow;
        }
    }

    return color;
}

// Central burst effect on kick
vec3 render_burst(vec2 uv, float t) {
    vec3 color = vec3(0.0);

    // Shockwave rings expanding from center
    float burst_time = mod(t * 2.0, 1.0);

    for (int i = 0; i < 5; i++) {
        float ring_offset = float(i) * 0.15;
        float ring_t = fract(burst_time + ring_offset);
        float ring_radius = ring_t * 1.5;

        float dist = abs(length(uv) - ring_radius);
        float ring_intensity = 0.015 / (dist + 0.01);
        ring_intensity *= (1.0 - ring_t);
        ring_intensity *= beat_energy * 0.7;

        // Color shifts
        vec3 ring_color = hsv2rgb(vec3(ring_t * 0.5 + t * 0.2, 0.9, 0.8));
        color += ring_color * ring_intensity;
    }

    // Central flash on beat
    if (is_beat == 1) {
        float flash = exp(-length(uv) * 8.0);
        color += vec3(0.3, 0.8, 1.0) * flash * 1.0;
    }

    return color;
}

// Spiral arms for visual interest
vec3 render_spirals(vec2 uv, float t) {
    vec3 color = vec3(0.0);

    float angle = atan(uv.y, uv.x);
    float radius = length(uv);

    // Logarithmic spiral
    for (int i = 0; i < 3; i++) {
        float offset = float(i) * 2.094; // 120 degrees
        float spiral = angle + offset - t * 0.5 + log(radius + 0.1) * 3.0;
        spiral = mod(spiral, 6.28) - 3.14;

        float line = abs(spiral);
        float width = 0.1 + spectrum[i * 20] * 0.15;

        if (line < width) {
            float intensity = (1.0 - line / width) * (1.0 / (radius + 0.5));
            intensity *= spectrum[i * 15] * 2.0;

            vec3 spiral_color = hsv2rgb(vec3(float(i) / 3.0 + t * 0.1, 0.7, 1.0));
            color += spiral_color * intensity;
        }
    }

    return color;
}

void main() {
    vec2 uv = (gl_FragCoord.xy - 0.5 * resolution) / resolution.y;

    // Layered rendering
    vec3 color = vec3(0.0);

    // Background spirals (subtle)
    color += render_spirals(uv, time) * 0.3;

    // Burst shockwaves
    color += render_burst(uv, time);

    // Particles (main attraction)
    color += render_particles(uv, time);

    // Add bloom/glow
    float brightness = dot(color, vec3(0.299, 0.587, 0.114));
    color += color * brightness * 0.5;

    // Vignette
    float vignette = 1.0 - length(uv) * 0.3;
    color *= vignette;

    // Bass thump darkens everything then releases
    float bass_pump = spectrum[0] * 0.3;
    color *= (1.0 - bass_pump * 0.5) + bass_pump * 1.5;

    FragColor = vec4(color, 1.0);
}
`,
};

export type WebGLShaderName = keyof typeof SHADERS;
export const SHADER_NAMES = Object.keys(SHADERS) as WebGLShaderName[];
