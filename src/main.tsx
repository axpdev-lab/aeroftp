import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { I18nProvider } from './i18n';
import { ActivityLogProvider } from './hooks/useActivityLog';
import { IconThemeProvider } from './hooks/useIconTheme';
import { ErrorBoundary } from './components/ErrorBoundary';
import { loader } from '@monaco-editor/react';
import './styles.css';

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
          <IconThemeProvider>
            <App />
          </IconThemeProvider>
        </ActivityLogProvider>
      </ErrorBoundary>
    </I18nProvider>
  </React.StrictMode>
);