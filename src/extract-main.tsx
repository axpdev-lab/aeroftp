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
import { ErrorBoundary } from './components/ErrorBoundary';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <ErrorBoundary>
        <ExtractWindow />
      </ErrorBoundary>
    </I18nProvider>
  </React.StrictMode>
);
