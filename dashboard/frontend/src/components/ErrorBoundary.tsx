import { Component, type ErrorInfo, type ReactNode } from 'react'

interface Props {
  children: ReactNode
  /** Bump this (e.g. the active tab) to clear the error when the view changes. */
  resetKey?: unknown
}
interface State {
  error: Error | null
}

/**
 * Catches render errors so one malformed payload can't white-screen the whole
 * dashboard; the nav stays live and the user can switch tabs or retry.
 */
export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null }

  static getDerivedStateFromError(error: Error): State {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('Dashboard render error:', error, info.componentStack)
  }

  componentDidUpdate(prev: Props) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null })
    }
  }

  render() {
    if (this.state.error) {
      return (
        <div className="mx-auto max-w-[1080px] px-6 py-16">
          <div className="panel p-8">
            <h2 className="mb-2 font-mono text-[12px] tracking-wide text-alert">VIEW FAILED TO RENDER</h2>
            <p className="mb-4 text-[13px] text-muted">
              This tab hit an unexpected error. The rest of the dashboard is still live, so switch tabs or retry.
            </p>
            <pre className="mb-4 overflow-x-auto rounded bg-ink/60 p-3 text-[11px] text-faint">
              {this.state.error.message}
            </pre>
            <button
              onClick={() => this.setState({ error: null })}
              className="rounded border border-line px-3 py-1.5 font-mono text-[12px] text-muted transition-colors hover:text-text"
            >
              Retry
            </button>
          </div>
        </div>
      )
    }
    return this.props.children
  }
}
