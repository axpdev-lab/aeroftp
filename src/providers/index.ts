/**
 * Providers Module - Cloud Storage Provider Registry
 * 
 * Exports the provider registry and types for use throughout the app.
 * 
 * Usage:
 *   import { providerRegistry, getProviderById } from '@/providers';
 *   
 *   const backblaze = getProviderById('backblaze');
 *   const s3Providers = providerRegistry.getByCategory('s3');
 */

// Types
export * from './types';

// Registry
export {
    PROVIDERS,
    providerRegistry,
    getProviderById,
    getProvidersByCategory,
    getAllProviders,
    getStableProviders,
} from './registry';
