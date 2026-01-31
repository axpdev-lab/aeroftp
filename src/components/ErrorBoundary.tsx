import React from 'react';
import { AlertTriangle, RefreshCw } from 'lucide-react';

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

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
        <div style={{
          display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center',
          height: '100vh', backgroundColor: '#0f172a', color: '#e2e8f0', fontFamily: 'system-ui, sans-serif',
          padding: '2rem', textAlign: 'center',
        }}>
          <AlertTriangle size={48} color="#f59e0b" style={{ marginBottom: '1rem' }} />
          <h1 style={{ fontSize: '1.5rem', marginBottom: '0.5rem' }}>Something went wrong</h1>
          <p style={{ color: '#94a3b8', marginBottom: '1.5rem', maxWidth: '500px' }}>
            AeroFTP encountered an unexpected error. You can try to recover or reload the application.
          </p>
          {this.state.error && (
            <pre style={{
              backgroundColor: '#1e293b', padding: '1rem', borderRadius: '0.5rem',
              fontSize: '0.75rem', color: '#f87171', maxWidth: '600px', overflow: 'auto',
              marginBottom: '1.5rem', textAlign: 'left',
            }}>
              {this.state.error.message}
            </pre>
          )}
          <div style={{ display: 'flex', gap: '0.75rem' }}>
            <button onClick={this.handleDismiss} style={{
              padding: '0.5rem 1.25rem', borderRadius: '0.375rem', border: '1px solid #334155',
              backgroundColor: '#1e293b', color: '#e2e8f0', cursor: 'pointer', fontSize: '0.875rem',
            }}>
              Try to recover
            </button>
            <button onClick={this.handleReload} style={{
              padding: '0.5rem 1.25rem', borderRadius: '0.375rem', border: 'none',
              backgroundColor: '#3b82f6', color: '#fff', cursor: 'pointer', fontSize: '0.875rem',
              display: 'flex', alignItems: 'center', gap: '0.375rem',
            }}>
              <RefreshCw size={14} /> Reload
            </button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
