/**
 * App Background Patterns
 * SVG patterns for the main app background (behind Quick Connect and panels)
 *
 * Includes geometric, organic/flowing, and tech-inspired patterns.
 * Opacity is set at ~0.12 in SVG; dark mode uses CSS opacity-40 to scale down.
 */

export interface AppBackgroundPattern {
    id: string;
    nameKey: string;
    svg: string;
}

export const APP_BACKGROUND_PATTERNS: AppBackgroundPattern[] = [
    // === Original Lock Screen Patterns (adapted) ===
    {
        id: 'cross',
        nameKey: 'appearance.patternCross',
        svg: `url("data:image/svg+xml,%3Csvg width='60' height='60' viewBox='0 0 60 60' xmlns='http://www.w3.org/2000/svg'%3E%3Cg fill='none' fill-rule='evenodd'%3E%3Cg fill='%23ffffff' fill-opacity='0.12'%3E%3Cpath d='M36 34v-4h-2v4h-4v2h4v4h2v-4h4v-2h-4zm0-30V0h-2v4h-4v2h4v4h2V6h4V4h-4zM6 34v-4H4v4H0v2h4v4h2v-4h4v-2H6zM6 4V0H4v4H0v2h4v4h2V6h4V4H6z'/%3E%3C/g%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'dots',
        nameKey: 'appearance.patternDots',
        svg: `url("data:image/svg+xml,%3Csvg width='20' height='20' viewBox='0 0 20 20' xmlns='http://www.w3.org/2000/svg'%3E%3Ccircle cx='10' cy='10' r='1.5' fill='%23ffffff' fill-opacity='0.12'/%3E%3C/svg%3E")`,
    },
    {
        id: 'circuit',
        nameKey: 'appearance.patternCircuit',
        svg: `url("data:image/svg+xml,%3Csvg width='80' height='80' viewBox='0 0 80 80' xmlns='http://www.w3.org/2000/svg'%3E%3Cg fill='none' stroke='%23ffffff' stroke-width='0.5' opacity='0.12'%3E%3Crect x='10' y='10' width='20' height='20' rx='2'/%3E%3Crect x='50' y='50' width='20' height='20' rx='2'/%3E%3Cline x1='30' y1='20' x2='50' y2='20'/%3E%3Cline x1='50' y1='20' x2='50' y2='50'/%3E%3Cline x1='20' y1='30' x2='20' y2='50'/%3E%3Cline x1='20' y1='50' x2='50' y2='50'/%3E%3Ccircle cx='30' cy='20' r='2' fill='%23ffffff'/%3E%3Ccircle cx='50' cy='50' r='2' fill='%23ffffff'/%3E%3Ccircle cx='20' cy='30' r='2' fill='%23ffffff'/%3E%3Ccircle cx='20' cy='50' r='2' fill='%23ffffff'/%3E%3C/g%3E%3C/svg%3E")`,
    },
    {
        id: 'diagonal',
        nameKey: 'appearance.patternDiagonal',
        svg: `url("data:image/svg+xml,%3Csvg width='40' height='40' viewBox='0 0 40 40' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M0 40L40 0M-10 10L10-10M30 50L50 30' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.12'/%3E%3C/svg%3E")`,
    },
    {
        id: 'hexagon',
        nameKey: 'appearance.patternHexagon',
        svg: `url("data:image/svg+xml,%3Csvg width='28' height='49' viewBox='0 0 28 49' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M14 0L28 8.5V25.5L14 34L0 25.5V8.5L14 0zM14 15L28 23.5V40.5L14 49L0 40.5V23.5L14 15z' stroke='%23ffffff' stroke-width='0.4' fill='none' opacity='0.12'/%3E%3C/svg%3E")`,
    },
    {
        id: 'grid',
        nameKey: 'appearance.patternGrid',
        svg: `url("data:image/svg+xml,%3Csvg width='40' height='40' viewBox='0 0 40 40' xmlns='http://www.w3.org/2000/svg'%3E%3Crect width='40' height='40' fill='none' stroke='%23ffffff' stroke-width='0.3' opacity='0.12'/%3E%3C/svg%3E")`,
    },
    {
        id: 'topography',
        nameKey: 'appearance.patternTopography',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M10 50c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.12'/%3E%3Cpath d='M10 30c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.10'/%3E%3Cpath d='M10 70c10-20 30-20 40 0s30 20 40 0' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.10'/%3E%3C/svg%3E")`,
    },

    // === NEW Patterns ===

    // Waves - Organic flowing curves
    {
        id: 'waves',
        nameKey: 'appearance.patternWaves',
        svg: `url("data:image/svg+xml,%3Csvg width='120' height='60' viewBox='0 0 120 60' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M0 30 Q30 10 60 30 T120 30' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.12'/%3E%3Cpath d='M0 45 Q30 25 60 45 T120 45' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.10'/%3E%3Cpath d='M0 15 Q30 -5 60 15 T120 15' stroke='%23ffffff' stroke-width='0.5' fill='none' opacity='0.10'/%3E%3C/svg%3E")`,
    },

    // Constellation - Connected dots like stars
    {
        id: 'constellation',
        nameKey: 'appearance.patternConstellation',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cg opacity='0.12'%3E%3Ccircle cx='20' cy='20' r='1.5' fill='%23ffffff'/%3E%3Ccircle cx='80' cy='30' r='1' fill='%23ffffff'/%3E%3Ccircle cx='50' cy='50' r='2' fill='%23ffffff'/%3E%3Ccircle cx='30' cy='80' r='1' fill='%23ffffff'/%3E%3Ccircle cx='90' cy='70' r='1.5' fill='%23ffffff'/%3E%3Ccircle cx='10' cy='60' r='1' fill='%23ffffff'/%3E%3Cline x1='20' y1='20' x2='50' y2='50' stroke='%23ffffff' stroke-width='0.3'/%3E%3Cline x1='80' y1='30' x2='50' y2='50' stroke='%23ffffff' stroke-width='0.3'/%3E%3Cline x1='50' y1='50' x2='30' y2='80' stroke='%23ffffff' stroke-width='0.3'/%3E%3Cline x1='50' y1='50' x2='90' y2='70' stroke='%23ffffff' stroke-width='0.3'/%3E%3Cline x1='20' y1='20' x2='10' y2='60' stroke='%23ffffff' stroke-width='0.3'/%3E%3C/g%3E%3C/svg%3E")`,
    },

    // Isometric - 3D cube pattern
    {
        id: 'isometric',
        nameKey: 'appearance.patternIsometric',
        svg: `url("data:image/svg+xml,%3Csvg width='60' height='52' viewBox='0 0 60 52' xmlns='http://www.w3.org/2000/svg'%3E%3Cg stroke='%23ffffff' stroke-width='0.4' fill='none' opacity='0.12'%3E%3Cpath d='M30 0 L60 17.3 L60 52 L30 34.6 L0 52 L0 17.3 Z'/%3E%3Cpath d='M30 0 L30 34.6'/%3E%3Cpath d='M0 17.3 L30 34.6 L60 17.3'/%3E%3C/g%3E%3C/svg%3E")`,
    },

    // Bubbles - Soft organic circles
    {
        id: 'bubbles',
        nameKey: 'appearance.patternBubbles',
        svg: `url("data:image/svg+xml,%3Csvg width='100' height='100' viewBox='0 0 100 100' xmlns='http://www.w3.org/2000/svg'%3E%3Cg stroke='%23ffffff' stroke-width='0.4' fill='none' opacity='0.10'%3E%3Ccircle cx='25' cy='25' r='20'/%3E%3Ccircle cx='75' cy='35' r='15'/%3E%3Ccircle cx='50' cy='75' r='22'/%3E%3Ccircle cx='85' cy='80' r='10'/%3E%3Ccircle cx='10' cy='70' r='8'/%3E%3C/g%3E%3C/svg%3E")`,
    },

    // None
    {
        id: 'none',
        nameKey: 'appearance.patternNone',
        svg: '',
    },
];

export const APP_BACKGROUND_KEY = 'aeroftp_app_background_pattern';
export const DEFAULT_APP_BACKGROUND = 'hexagon';
