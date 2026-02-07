/**
 * Open URL in default browser using Tauri shell plugin
 * This is required in Tauri 2.0 since target="_blank" doesn't work
 *
 * Security: only http, https and mailto schemes are allowed.
 * Other schemes (file://, javascript:, custom://) are blocked to prevent abuse.
 */
import { open } from '@tauri-apps/plugin-shell';

const ALLOWED_SCHEMES = ['http:', 'https:', 'mailto:'];

export async function openUrl(url: string): Promise<void> {
  try {
    const parsed = new URL(url);
    if (!ALLOWED_SCHEMES.includes(parsed.protocol)) {
      throw new Error(`Blocked URL scheme: ${parsed.protocol}`);
    }
    await open(url);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith('Blocked URL scheme')) {
      console.warn(error.message);
      return;
    }
    console.error('Failed to open URL:', error);
    // Fallback: try window.open (won't work in Tauri but useful for dev)
    window.open(url, '_blank');
  }
}

export default openUrl;
