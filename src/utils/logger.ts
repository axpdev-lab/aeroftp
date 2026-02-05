/**
 * Debug-gated logger utility
 *
 * In development: all levels are active.
 * In production: only warn and error are active; debug/info are no-ops.
 *
 * Usage:
 *   import { logger } from '../utils/logger';
 *   logger.debug('[Module]', 'message', data);
 *   logger.info('[Module]', 'message');
 *   logger.warn('[Module]', 'message', error);
 *   logger.error('[Module]', 'message', error);
 */

const isDev = import.meta.env.DEV;

const noop = (..._args: unknown[]) => {};

export const logger = {
    debug: isDev ? console.log.bind(console) : noop,
    info: isDev ? console.log.bind(console) : noop,
    warn: console.warn.bind(console),
    error: console.error.bind(console),
};
