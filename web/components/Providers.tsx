'use client'

import { ToastContainer } from './ui/Toast'
import { ErrorBoundary, PageErrorFallback } from './ui/ErrorBoundary'

export function Providers({ children }: { children: React.ReactNode }) {
  return (
    <ErrorBoundary fallback={<PageErrorFallback />}>
      {children}
      <ToastContainer />
    </ErrorBoundary>
  )
}
