// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// i18n Public Exports
// Clean API for consuming i18n functionality throughout the app

export { I18nProvider, useI18n, useTranslation } from './I18nContext';

export type {
    Language,
    TranslationKeys,
    TranslationFunction,
    I18nContextValue,
    LanguageInfo,
} from './types';

export {
    AVAILABLE_LANGUAGES,
    DEFAULT_LANGUAGE,
    LANGUAGE_STORAGE_KEY,
} from './types';
