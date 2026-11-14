// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { I18nProvider } from './i18n';
import { ActivityLogProvider } from './hooks/useActivityLog';
import { NotificationCenterProvider } from './hooks/useNotificationCenter';
import { IconThemeProvider } from './hooks/useIconTheme';
import { ErrorBoundary } from './components/ErrorBoundary';
import { loader } from '@monaco-editor/react';
import './styles.css';
import { invoke } from '@tauri-apps/api/core';
import { bindSyncPage } from './utils/remoteSyncRunner';

// Bind this document to its window before anything else can ask for a sync
// run (the native side gives runs only to the window's last bound document).
// A run awaits this same binding; a failure here is retried by the next run.
void bindSyncPage(invoke).catch(() => undefined);

// Pre-configure Monaco AMD path (must match CodeEditor.tsx)
loader.config({ paths: { vs: '/vs' } });

// Warm up Monaco in background during idle time so first "View Source" is instant
if ('requestIdleCallback' in window) {
  requestIdleCallback(() => { loader.init().catch(() => {}); }, { timeout: 5000 });
} else {
  setTimeout(() => { loader.init().catch(() => {}); }, 2000);
}

// Render the app with i18n, activity log, and icon theme support
ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <ErrorBoundary>
        <ActivityLogProvider>
          <NotificationCenterProvider>
            <IconThemeProvider>
              <App />
            </IconThemeProvider>
          </NotificationCenterProvider>
        </ActivityLogProvider>
      </ErrorBoundary>
    </I18nProvider>
  </React.StrictMode>
);