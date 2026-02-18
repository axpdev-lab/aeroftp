import { logger } from './logger';

/**
 * CSP Phase 2: Client-side violation reporting.
 * Logs Content-Security-Policy violations via the debug-gated logger.
 * In production, violations are silently ignored (logger is no-op).
 * Idempotent: safe to call multiple times (FE-007).
 */
let _cspInitialized = false;

export function initCspReporter(): void {
    if (_cspInitialized) return;
    _cspInitialized = true;

    document.addEventListener('securitypolicyviolation', (event) => {
        // Truncate blockedURI to avoid logging sensitive data (SEC-011)
        const blockedUri = event.blockedURI?.slice(0, 100) || '';
        logger.warn('[CSP Violation]', {
            directive: event.violatedDirective,
            blockedUri,
            sourceFile: event.sourceFile,
            lineNumber: event.lineNumber,
            columnNumber: event.columnNumber,
        });
    });
}
