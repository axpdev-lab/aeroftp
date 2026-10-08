// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Entry point for the AeroAgent approval window. Like the extract window it
// mounts only its own component and the i18n provider, never the main App: the
// window must not share anything with the chat it is approving for.

import React from 'react';
import ReactDOM from 'react-dom/client';
import AiApprovalWindow from './components/AiApprovalWindow';
import { I18nProvider } from './i18n';
import { ErrorBoundary } from './components/ErrorBoundary';
import { useTheme } from './hooks/useTheme';
import { applyThemeClasses, readAppearance, resolveAppearance } from './utils/appearance';
import './styles.css';

const appearance = readAppearance();
applyThemeClasses(resolveAppearance(appearance.preference, appearance.schedule, window.matchMedia('(prefers-color-scheme: dark)').matches));

function WindowTheme({ children }: { children: React.ReactNode }) {
  // This approval surface has no event IPC capability. Storage, focus and
  // minute refreshes keep its palette live without expanding its permissions.
  useTheme({ ipcEvents: false });
  return <>{children}</>;
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <WindowTheme>
        <ErrorBoundary>
          <AiApprovalWindow />
        </ErrorBoundary>
      </WindowTheme>
    </I18nProvider>
  </React.StrictMode>
);
