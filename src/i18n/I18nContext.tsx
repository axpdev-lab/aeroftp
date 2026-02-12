// i18n Context Provider
// Lightweight React Context-based internationalization system
// Supports 47 languages

import React, { createContext, useContext, useState, useEffect, useCallback, useMemo } from 'react';
import {
    Language,
    I18nContextValue,
    TranslationFunction,
    TranslationKeys,
    AVAILABLE_LANGUAGES,
    DEFAULT_LANGUAGE,
    LANGUAGE_STORAGE_KEY,
} from './types';

// Import translations statically for bundle optimization
// Using static imports ensures tree-shaking and type safety

// Original 5 languages
import enTranslations from './locales/en.json';
import itTranslations from './locales/it.json';
import esTranslations from './locales/es.json';
import frTranslations from './locales/fr.json';
import zhTranslations from './locales/zh.json';

// Major European (12)
import deTranslations from './locales/de.json';
import ptTranslations from './locales/pt.json';
import ruTranslations from './locales/ru.json';
import nlTranslations from './locales/nl.json';
import plTranslations from './locales/pl.json';
import ukTranslations from './locales/uk.json';
import roTranslations from './locales/ro.json';
import csTranslations from './locales/cs.json';
import huTranslations from './locales/hu.json';
import elTranslations from './locales/el.json';
import bgTranslations from './locales/bg.json';
import skTranslations from './locales/sk.json';

// Nordic (5)
import svTranslations from './locales/sv.json';
import daTranslations from './locales/da.json';
import noTranslations from './locales/no.json';
import fiTranslations from './locales/fi.json';
import isTranslations from './locales/is.json';

// Asian (10)
import jaTranslations from './locales/ja.json';
import koTranslations from './locales/ko.json';
import viTranslations from './locales/vi.json';
import thTranslations from './locales/th.json';
import idTranslations from './locales/id.json';
import msTranslations from './locales/ms.json';
import tlTranslations from './locales/tl.json';
import kmTranslations from './locales/km.json';
import hiTranslations from './locales/hi.json';
import bnTranslations from './locales/bn.json';

// Balkan & Caucasus (6)
import hrTranslations from './locales/hr.json';
import srTranslations from './locales/sr.json';
import slTranslations from './locales/sl.json';
import mkTranslations from './locales/mk.json';
import kaTranslations from './locales/ka.json';
import hyTranslations from './locales/hy.json';

// Baltic (3)
import ltTranslations from './locales/lt.json';
import lvTranslations from './locales/lv.json';
import etTranslations from './locales/et.json';

// Celtic & Iberian (4)
import cyTranslations from './locales/cy.json';
import glTranslations from './locales/gl.json';
import caTranslations from './locales/ca.json';
import euTranslations from './locales/eu.json';

// African (1)
import swTranslations from './locales/sw.json';

// Turkish (1)
import trTranslations from './locales/tr.json';

// Translation map for O(1) lookup - 47 languages
const TRANSLATIONS: Record<Language, { translations: TranslationKeys }> = {
    // Original 5
    en: enTranslations as { translations: TranslationKeys },
    it: itTranslations as { translations: TranslationKeys },
    es: esTranslations as { translations: TranslationKeys },
    fr: frTranslations as { translations: TranslationKeys },
    zh: zhTranslations as { translations: TranslationKeys },
    // Major European (12)
    de: deTranslations as { translations: TranslationKeys },
    pt: ptTranslations as { translations: TranslationKeys },
    ru: ruTranslations as { translations: TranslationKeys },
    nl: nlTranslations as { translations: TranslationKeys },
    pl: plTranslations as { translations: TranslationKeys },
    uk: ukTranslations as { translations: TranslationKeys },
    ro: roTranslations as { translations: TranslationKeys },
    cs: csTranslations as { translations: TranslationKeys },
    hu: huTranslations as { translations: TranslationKeys },
    el: elTranslations as { translations: TranslationKeys },
    bg: bgTranslations as { translations: TranslationKeys },
    sk: skTranslations as { translations: TranslationKeys },
    // Nordic (5)
    sv: svTranslations as { translations: TranslationKeys },
    da: daTranslations as { translations: TranslationKeys },
    no: noTranslations as { translations: TranslationKeys },
    fi: fiTranslations as { translations: TranslationKeys },
    is: isTranslations as { translations: TranslationKeys },
    // Asian (10)
    ja: jaTranslations as { translations: TranslationKeys },
    ko: koTranslations as { translations: TranslationKeys },
    vi: viTranslations as { translations: TranslationKeys },
    th: thTranslations as { translations: TranslationKeys },
    id: idTranslations as { translations: TranslationKeys },
    ms: msTranslations as { translations: TranslationKeys },
    tl: tlTranslations as { translations: TranslationKeys },
    km: kmTranslations as { translations: TranslationKeys },
    hi: hiTranslations as { translations: TranslationKeys },
    bn: bnTranslations as { translations: TranslationKeys },
    // Balkan & Caucasus (6)
    hr: hrTranslations as { translations: TranslationKeys },
    sr: srTranslations as { translations: TranslationKeys },
    sl: slTranslations as { translations: TranslationKeys },
    mk: mkTranslations as { translations: TranslationKeys },
    ka: kaTranslations as { translations: TranslationKeys },
    hy: hyTranslations as { translations: TranslationKeys },
    // Baltic (3)
    lt: ltTranslations as { translations: TranslationKeys },
    lv: lvTranslations as { translations: TranslationKeys },
    et: etTranslations as { translations: TranslationKeys },
    // Celtic & Iberian (4)
    cy: cyTranslations as { translations: TranslationKeys },
    gl: glTranslations as { translations: TranslationKeys },
    ca: caTranslations as { translations: TranslationKeys },
    eu: euTranslations as { translations: TranslationKeys },
    // African (1)
    sw: swTranslations as { translations: TranslationKeys },
    // Turkish (1)
    tr: trTranslations as { translations: TranslationKeys },
};

// Create context with undefined default (will throw if used outside provider)
const I18nContext = createContext<I18nContextValue | undefined>(undefined);

/**
 * Get nested value from object using dot notation
 * Example: getNestedValue(obj, 'common.save') -> obj.common.save
 */
function getNestedValue(obj: Record<string, unknown>, path: string): string | undefined {
    const keys = path.split('.');
    let current: unknown = obj;

    for (const key of keys) {
        if (current === null || current === undefined || typeof current !== 'object') {
            return undefined;
        }
        current = (current as Record<string, unknown>)[key];
    }

    return typeof current === 'string' ? current : undefined;
}

/**
 * Replace template parameters in translation string
 * Example: interpolate('Hello {name}!', { name: 'World' }) -> 'Hello World!'
 */
function interpolate(template: string, params?: Record<string, string | number>): string {
    if (!params) return template;

    return template.replace(/\{(\w+)\}/g, (match, key) => {
        const value = params[key];
        return value !== undefined ? String(value) : match;
    });
}

/**
 * Detect browser language preference (DISABLED for developer-first approach)
 * Now returns default language - users must explicitly choose their language
 * This ensures consistent English default across all systems
 */
function detectBrowserLanguage(): Language {
    // Developer-first app: default to English, let users choose their preferred language
    return DEFAULT_LANGUAGE;
}

/**
 * Load persisted language preference from localStorage
 */
function loadPersistedLanguage(): Language | null {
    try {
        const stored = localStorage.getItem(LANGUAGE_STORAGE_KEY);
        if (stored && AVAILABLE_LANGUAGES.some(l => l.code === stored)) {
            return stored as Language;
        }
    } catch {
        // localStorage not available (SSR or privacy mode)
    }
    return null;
}

/**
 * Persist language preference to localStorage
 */
function persistLanguage(language: Language): void {
    try {
        localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
    } catch {
        // Ignore storage errors
    }
}

/**
 * I18n Provider Props
 */
interface I18nProviderProps {
    children: React.ReactNode;
    initialLanguage?: Language;
}

/**
 * I18n Provider Component
 * Wraps the application and provides translation context
 */
export const I18nProvider: React.FC<I18nProviderProps> = ({ children, initialLanguage }) => {
    // Initialize language: prop > localStorage > browser detection > default
    const [language, setLanguageState] = useState<Language>(() => {
        if (initialLanguage) return initialLanguage;
        return loadPersistedLanguage() || detectBrowserLanguage();
    });

    // Memoized translations for current language
    const translations = useMemo(() => {
        return TRANSLATIONS[language]?.translations || TRANSLATIONS[DEFAULT_LANGUAGE].translations;
    }, [language]);

    // Fallback translations (English) for missing keys
    const fallbackTranslations = useMemo(() => {
        return TRANSLATIONS[DEFAULT_LANGUAGE].translations;
    }, []);

    /**
     * Translation function
     * Supports dot notation: t('common.save')
     * Supports interpolation: t('toast.connectionSuccess', { server: 'ftp.example.com' })
     */
    const t: TranslationFunction = useCallback(
        (key: string, params?: Record<string, string | number>) => {
            // Try current language first
            let value = getNestedValue(translations as unknown as Record<string, unknown>, key);

            // Fallback to English if key not found
            if (value === undefined && language !== DEFAULT_LANGUAGE) {
                value = getNestedValue(fallbackTranslations as unknown as Record<string, unknown>, key);
            }

            // Return key if translation not found (helps identify missing translations)
            if (value === undefined) {
                console.warn(`[i18n] Missing translation: ${key}`);
                return key;
            }

            // Apply parameter interpolation
            return interpolate(value, params);
        },
        [translations, fallbackTranslations, language]
    );

    /**
     * Set language and persist preference
     */
    const setLanguage = useCallback((newLanguage: Language) => {
        if (!AVAILABLE_LANGUAGES.some(l => l.code === newLanguage)) {
            console.warn(`[i18n] Unsupported language: ${newLanguage}`);
            return;
        }

        setLanguageState(newLanguage);
        persistLanguage(newLanguage);

        // Emit custom event for components that need to react to language changes
        window.dispatchEvent(new CustomEvent('aeroftp-language-changed', { detail: newLanguage }));
    }, []);

    // Update document lang attribute for accessibility
    useEffect(() => {
        document.documentElement.lang = language;
    }, [language]);

    // Memoize context value to prevent unnecessary re-renders
    const contextValue = useMemo<I18nContextValue>(
        () => ({
            language,
            setLanguage,
            t,
            availableLanguages: AVAILABLE_LANGUAGES,
        }),
        [language, setLanguage, t]
    );

    return (
        <I18nContext.Provider value={contextValue}>
            {children}
        </I18nContext.Provider>
    );
};

/**
 * Hook to access i18n context
 * Must be used within an I18nProvider
 */
export function useI18n(): I18nContextValue {
    const context = useContext(I18nContext);
    if (context === undefined) {
        throw new Error('useI18n must be used within an I18nProvider');
    }
    return context;
}

/**
 * Shorthand hook for translation function only
 * Use when you only need the t() function
 */
export function useTranslation(): TranslationFunction {
    const { t } = useI18n();
    return t;
}

export default I18nProvider;
