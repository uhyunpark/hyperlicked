'use client'

import { Component, type ErrorInfo, type ReactNode } from 'react'

interface ErrorBoundaryProps {
  children: ReactNode
  fallback?: ReactNode
  onError?: (error: Error, errorInfo: ErrorInfo) => void
}

interface ErrorBoundaryState {
  hasError: boolean
  error: Error | null
}

/**
 * Error Boundary Component
 *
 * Catches JavaScript errors anywhere in the child component tree,
 * logs those errors, and displays a fallback UI instead of crashing.
 *
 * Usage:
 * <ErrorBoundary fallback={<div>Something went wrong</div>}>
 *   <MyComponent />
 * </ErrorBoundary>
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props)
    this.state = { hasError: false, error: null }
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error }
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error('[ErrorBoundary] Caught error:', error, errorInfo)
    this.props.onError?.(error, errorInfo)
  }

  handleRetry = () => {
    this.setState({ hasError: false, error: null })
  }

  render() {
    if (this.state.hasError) {
      if (this.props.fallback) {
        return this.props.fallback
      }

      return (
        <div className="flex h-full min-h-[200px] flex-col items-center justify-center gap-4 rounded-lg border border-border bg-bg-secondary p-8">
          <div className="text-center">
            <h2 className="mb-2 text-lg font-semibold text-text-primary">
              Something went wrong
            </h2>
            <p className="mb-4 text-sm text-text-muted">
              {this.state.error?.message || 'An unexpected error occurred'}
            </p>
            <button
              onClick={this.handleRetry}
              className="rounded bg-accent px-4 py-2 text-sm font-medium text-white transition-opacity hover:opacity-90"
            >
              Try Again
            </button>
          </div>
        </div>
      )
    }

    return this.props.children
  }
}

/**
 * Error Fallback for the full page
 */
export function PageErrorFallback() {
  return (
    <div className="flex h-screen flex-col items-center justify-center gap-4 bg-bg-primary p-8">
      <div className="text-center">
        <h1 className="mb-2 text-2xl font-bold text-text-primary">
          Application Error
        </h1>
        <p className="mb-6 text-text-muted">
          Something went wrong. Please refresh the page to try again.
        </p>
        <button
          onClick={() => window.location.reload()}
          className="rounded bg-accent px-6 py-3 text-sm font-semibold text-white transition-opacity hover:opacity-90"
        >
          Refresh Page
        </button>
      </div>
    </div>
  )
}

/**
 * Error Fallback for trading sections
 */
export function SectionErrorFallback({ title }: { title: string }) {
  return (
    <div className="flex h-full min-h-[100px] items-center justify-center rounded border border-border bg-bg-secondary p-4">
      <p className="text-sm text-text-muted">
        Failed to load {title}. <button onClick={() => window.location.reload()} className="text-accent hover:underline">Refresh</button>
      </p>
    </div>
  )
}
