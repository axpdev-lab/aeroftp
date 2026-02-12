import React from 'react';
import { AlertTriangle, RefreshCw } from 'lucide-react';
import { useTranslation } from '../i18n';

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

// Functional component for error UI (can use hooks)
const ErrorFallback: React.FC<{
  error: Error | null;
  onDismiss: () => void;
  onReload: () => void;
}> = ({ error, onDismiss, onReload }) => {
  const t = useTranslation();

  return (
    <div style={{
      display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center',
      height: '100vh', backgroundColor: '#0f172a', color: '#e2e8f0', fontFamily: 'system-ui, sans-serif',
      padding: '2rem', textAlign: 'center',
    }}>
      <AlertTriangle size={48} color="#f59e0b" style={{ marginBottom: '1rem' }} />
      <h1 style={{ fontSize: '1.5rem', marginBottom: '0.5rem' }}>{t('error.title')}</h1>
      <p style={{ color: '#94a3b8', marginBottom: '1.5rem', maxWidth: '500px' }}>
        {t('error.description')}
      </p>
      {error && (
        <pre style={{
          backgroundColor: '#1e293b', padding: '1rem', borderRadius: '0.5rem',
          fontSize: '0.75rem', color: '#f87171', maxWidth: '600px', overflow: 'auto',
          marginBottom: '1.5rem', textAlign: 'left',
        }}>
          {error.message}
        </pre>
      )}
      <div style={{ display: 'flex', gap: '0.75rem' }}>
        <button onClick={onDismiss} style={{
          padding: '0.5rem 1.25rem', borderRadius: '0.375rem', border: '1px solid #334155',
          backgroundColor: '#1e293b', color: '#e2e8f0', cursor: 'pointer', fontSize: '0.875rem',
        }}>
          {t('error.tryRecover')}
        </button>
        <button onClick={onReload} style={{
          padding: '0.5rem 1.25rem', borderRadius: '0.375rem', border: 'none',
          backgroundColor: '#3b82f6', color: '#fff', cursor: 'pointer', fontSize: '0.875rem',
          display: 'flex', alignItems: 'center', gap: '0.375rem',
        }}>
          <RefreshCw size={14} /> {t('error.reload')}
        </button>
      </div>
    </div>
  );
};

export class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  ErrorBoundaryState
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error('[ErrorBoundary] Uncaught error:', error);
    console.error('[ErrorBoundary] Component stack:', errorInfo.componentStack);
  }

  handleReload = () => {
    window.location.reload();
  };

  handleDismiss = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <ErrorFallback
          error={this.state.error}
          onDismiss={this.handleDismiss}
          onReload={this.handleReload}
        />
      );
    }

    return this.props.children;
  }
}
