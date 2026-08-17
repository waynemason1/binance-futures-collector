import { useEffect, useState } from 'react'
import ErrorBoundary from './components/ErrorBoundary'
import Console from './pages/Console'
import Coverage from './pages/Coverage'
import Validation from './pages/Validation'
import Replay from './pages/Replay'
import Export from './pages/Export'
import Config from './pages/Config'
import Logs from './pages/Logs'

type Tab = 'console' | 'coverage' | 'validation' | 'replay' | 'export' | 'config' | 'logs'
const TABS: { id: Tab; label: string }[] = [
  { id: 'console', label: 'Console' },
  { id: 'coverage', label: 'Coverage' },
  { id: 'validation', label: 'Validation' },
  { id: 'replay', label: 'Replay' },
  { id: 'export', label: 'Export' },
  { id: 'config', label: 'Config' },
  { id: 'logs', label: 'Logs' },
]
const IDS = TABS.map((t) => t.id) as string[]

// Hash routing keeps each tab deep-linkable and refresh-stable without pulling in
// a router: a flat 7-tab set doesn't need one.
function tabFromHash(): Tab {
  const h = window.location.hash.replace(/^#\/?/, '')
  return (IDS.includes(h) ? h : 'console') as Tab
}

export default function App() {
  const [tab, setTab] = useState<Tab>(tabFromHash)

  useEffect(() => {
    const onHash = () => setTab(tabFromHash())
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  }, [])

  return (
    <div className="min-h-screen overflow-x-hidden">
      <nav className="sticky top-0 z-40 border-b border-line bg-ink/80 backdrop-blur">
        <div className="mx-auto flex max-w-[1080px] flex-wrap items-center gap-x-1 px-3 sm:px-6">
          {TABS.map((t) => (
            <a
              key={t.id}
              href={`#${t.id}`}
              aria-current={tab === t.id ? 'page' : undefined}
              className={`relative px-3 py-3 font-mono text-[11.5px] tracking-wide transition-colors sm:px-4 sm:text-[12px] ${
                tab === t.id ? 'text-text' : 'text-faint hover:text-muted'
              }`}
            >
              {t.label.toUpperCase()}
              {tab === t.id && <span className="absolute inset-x-3 -bottom-px h-0.5 bg-accent" />}
            </a>
          ))}
        </div>
      </nav>

      <ErrorBoundary resetKey={tab}>
        {tab === 'console' && <Console />}
        {tab === 'coverage' && <Coverage />}
        {tab === 'validation' && <Validation />}
        {tab === 'replay' && <Replay />}
        {tab === 'export' && <Export />}
        {tab === 'config' && <Config />}
        {tab === 'logs' && <Logs />}
      </ErrorBoundary>
    </div>
  )
}
