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
import type { Theme } from './hooks/useTheme';
import './styles.css';

const savedTheme = (localStorage.getItem('aeroftp-theme') as Theme | null) ?? 'auto';
const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
const isDark = savedTheme === 'auto'
  ? prefersDark
  : ['dark', 'truedark', 'tokyo', 'cyber', 'green', 'redhorse'].includes(savedTheme);

document.documentElement.classList.toggle('dark', isDark);
document.documentElement.classList.toggle('truedark', savedTheme === 'truedark');

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <ErrorBoundary>
        <AiApprovalWindow />
      </ErrorBoundary>
    </I18nProvider>
  </React.StrictMode>
);
