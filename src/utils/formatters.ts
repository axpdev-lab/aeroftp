/**
 * Utility formatting functions
 */

/**
 * Format bytes to human-readable string
 */
export const formatBytes = (bytes: number): string => {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
};

/**
 * Format transfer speed
 */
export const formatSpeed = (bps: number): string => formatBytes(bps) + '/s';

/**
 * Format estimated time remaining
 */
export const formatETA = (seconds: number): string => {
    if (seconds < 60) return `${Math.round(seconds)}s`;
    if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
    return `${Math.round(seconds / 3600)}h`;
};

/**
 * Format date string using browser locale (Intl.DateTimeFormat)
 * Automatically adapts to user's language/region settings
 * Input formats:
 *   - ISO: "2025-12-17 00:36" or "2025-12-17 00:36:45"
 *   - FTP Unix: "Feb 3 01:34" or "Dec 14 2024"
 *   - DOS: "02-03-25 01:34"
 *   - Date object
 * Output: locale-formatted date like "17 dic 2025, 00:36" (IT) or "Dec 17, 2025, 12:36 AM" (US)
 */
export const formatDate = (dateStr: string | Date | null): string => {
    if (!dateStr) return '';

    let date: Date;

    if (dateStr instanceof Date) {
        date = dateStr;
    } else {
        const str = dateStr.trim();

        // 1. Try ISO format: YYYY-MM-DD HH:MM or YYYY-MM-DD HH:MM:SS
        const isoMatch = str.match(/^(\d{4})-(\d{2})-(\d{2})\s+(\d{2}):(\d{2})(?::(\d{2}))?/);
        if (isoMatch) {
            const [, year, month, day, hour, minute, second] = isoMatch;
            date = new Date(
                parseInt(year),
                parseInt(month) - 1,
                parseInt(day),
                parseInt(hour),
                parseInt(minute),
                parseInt(second || '0')
            );
        }
        // 2. Try FTP Unix format: "Feb 3 01:34" or "Dec 14 2024"
        else {
            const months: Record<string, number> = {
                Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5,
                Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11
            };
            const ftpMatch = str.match(/^([A-Za-z]{3})\s+(\d{1,2})\s+(\d{2}:\d{2}|\d{4})$/);
            if (ftpMatch) {
                const [, monthStr, dayStr, timeOrYear] = ftpMatch;
                const monthIndex = months[monthStr];
                const day = parseInt(dayStr);
                const currentYear = new Date().getFullYear();

                if (timeOrYear.includes(':')) {
                    // Format: "Feb 3 01:34" - current year assumed
                    const [hour, minute] = timeOrYear.split(':').map(Number);
                    date = new Date(currentYear, monthIndex, day, hour, minute);
                } else {
                    // Format: "Dec 14 2024" - year specified, time unknown
                    const year = parseInt(timeOrYear);
                    date = new Date(year, monthIndex, day, 0, 0);
                }
            } else {
                // Unable to parse - return original string
                return str;
            }
        }
    }

    if (isNaN(date.getTime())) return String(dateStr);

    // Use browser locale for formatting
    return new Intl.DateTimeFormat(navigator.language, {
        day: '2-digit',
        month: 'short',
        year: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
        hour12: false
    }).format(date);
};

/**
 * @deprecated Use formatDate() instead - now uses Intl.DateTimeFormat for all locales
 */
export const formatDateCompact = formatDate;
