import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

const queryClient = new QueryClient()

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <div className="min-h-screen bg-gray-950 text-gray-100">
        <h1 className="text-2xl font-bold p-4">Auto Trader Dashboard</h1>
        <p className="px-4 text-gray-400">Loading...</p>
      </div>
    </QueryClientProvider>
  )
}

export default App
