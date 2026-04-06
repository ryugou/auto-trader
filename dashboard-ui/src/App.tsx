import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom'
import Overview from './pages/Overview'
import Trades from './pages/Trades'
import Analysis from './pages/Analysis'
import Accounts from './pages/Accounts'
import Positions from './pages/Positions'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: 1,
    },
  },
})

const navItems = [
  { to: '/', label: '概要' },
  { to: '/trades', label: 'トレード' },
  { to: '/analysis', label: '分析' },
  { to: '/accounts', label: '口座' },
  { to: '/positions', label: 'ポジション' },
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
          <header className="border-b border-gray-800 px-4 py-3">
            <div className="max-w-7xl mx-auto flex flex-col sm:flex-row items-start sm:items-center gap-3">
              <h1 className="text-lg font-bold whitespace-nowrap">
                Auto Trader
              </h1>
              <NavBar />
            </div>
          </header>
          <main className="max-w-7xl mx-auto p-4">
            <Routes>
              <Route path="/" element={<Overview />} />
              <Route path="/trades" element={<Trades />} />
              <Route path="/analysis" element={<Analysis />} />
              <Route path="/accounts" element={<Accounts />} />
              <Route path="/positions" element={<Positions />} />
            </Routes>
          </main>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  )
}

export default App
