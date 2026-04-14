import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom'
import Overview from './pages/Overview'
import Trades from './pages/Trades'
import Analysis from './pages/Analysis'
import Accounts from './pages/Accounts'
import Positions from './pages/Positions'
import Strategies from './pages/Strategies'
import Notifications from './pages/Notifications'
import NotificationBell from './components/NotificationBell'
import MarketFeedHealthBanner from './components/MarketFeedHealthBanner'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: 1,
      // Auto-refetch every 15 seconds while a query is mounted so the
      // dashboard reflects new positions / fills / balance changes
      // without the user having to hit reload. TanStack defaults to
      // pausing this when the browser tab is in the background, so we
      // don't burn CPU when nobody's watching.
      // (refetchOnWindowFocus is already the TanStack default — left
      // implicit so the config matches the actual behavior.)
      refetchInterval: 15_000,
    },
  },
})

const navItems = [
  { to: '/', label: '概要' },
  { to: '/positions', label: 'ポジション' },
  { to: '/trades', label: 'トレード' },
  { to: '/analysis', label: '分析' },
  { to: '/accounts', label: '口座' },
  { to: '/strategies', label: '戦略' },
]

function NavBar() {
  return (
    <nav className="flex items-center gap-1 overflow-x-auto">
      {navItems.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.to === '/'}
          className={({ isActive }) =>
            `px-3 py-1.5 text-sm rounded transition whitespace-nowrap ${
              isActive
                ? 'bg-gray-800 text-gray-100 font-medium'
                : 'text-gray-400 hover:text-gray-200 hover:bg-gray-800/50'
            }`
          }
        >
          {item.label}
        </NavLink>
      ))}
    </nav>
  )
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <div className="min-h-screen bg-gray-950 text-gray-100">
          <header className="border-b border-gray-800 px-[15px] py-3">
            <div className="flex flex-col sm:flex-row items-start sm:items-center gap-3">
              <h1 className="text-lg font-bold whitespace-nowrap">Auto Trader</h1>
              <NavBar />
              {/* Bell lives flush-right; `ml-auto` inside the component
                  pushes it to the end of the flex row. Deliberately not
                  in `navItems` so it does not render as a tab. */}
              <NotificationBell />
            </div>
          </header>
          <MarketFeedHealthBanner />
          <main className="px-[15px] py-4">
            <Routes>
              <Route path="/" element={<Overview />} />
              <Route path="/trades" element={<Trades />} />
              <Route path="/analysis" element={<Analysis />} />
              <Route path="/accounts" element={<Accounts />} />
              <Route path="/positions" element={<Positions />} />
              <Route path="/strategies" element={<Strategies />} />
              <Route path="/notifications" element={<Notifications />} />
            </Routes>
          </main>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  )
}

export default App
