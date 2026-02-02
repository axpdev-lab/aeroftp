import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { I18nProvider } from './i18n';
import { ActivityLogProvider } from './hooks/useActivityLog';
import { ErrorBoundary } from './components/ErrorBoundary';
import './styles.css';

// Render the app with i18n and activity log support
ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <I18nProvider>
        <ActivityLogProvider>
          <App />
        </ActivityLogProvider>
      </I18nProvider>
    </ErrorBoundary>
  </React.StrictMode>
);