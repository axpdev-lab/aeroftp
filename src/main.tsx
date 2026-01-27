import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { I18nProvider } from './i18n';
import { ActivityLogProvider } from './hooks/useActivityLog';
import './styles.css';

// Aptabase analytics is initialized on the Rust side with tauri-plugin-aptabase
// See src-tauri/src/lib.rs for initialization
// The hook useAnalytics respects user opt-in preference from Settings

// Render the app with i18n and activity log support
ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <ActivityLogProvider>
        <App />
      </ActivityLogProvider>
    </I18nProvider>
  </React.StrictMode>
);