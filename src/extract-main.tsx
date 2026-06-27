// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Entry point for the dedicated lightweight `extract` window (Deliverable G,
// Option B). Deliberately minimal: it mounts ONLY the ExtractWindow dialog and
// the i18n provider, never the main App. This is what lets the OS "Extract here
// / to folder" verbs show UI without booting the full app (no vault unlock, no
// sync workers, no Monaco). See docs/dev/deliverable-G/PHASE1-startup-measurement.

import React from 'react';
import ReactDOM from 'react-dom/client';
import ExtractWindow from './components/ExtractWindow';
import { I18nProvider } from './i18n';
import { AVAILABLE_LANGUAGES, type Language } from './i18n';
import { ErrorBoundary } from './components/ErrorBoundary';
import './styles.css';

// Match the OS language (injected by the Rust open_extract_window payload), so the
// window reads in the desktop language like the Nautilus verbs do, not whatever
// language the main app was last left in. Falls back to the I18nProvider default
// when the desktop language is not one we ship.
const desktopLang = (window as { __AEROFTP_EXTRACT__?: { lang?: string } }).__AEROFTP_EXTRACT__?.lang;
const initialLanguage = AVAILABLE_LANGUAGES.some((l) => l.code === desktopLang)
  ? (desktopLang as Language)
  : undefined;

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider initialLanguage={initialLanguage}>
      <ErrorBoundary>
        <ExtractWindow />
      </ErrorBoundary>
    </I18nProvider>
  </React.StrictMode>
);
