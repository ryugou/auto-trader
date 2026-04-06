import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

export default function Positions() {
  const queryClient = useQueryClient()

  const { data: positions, isLoading } = useQuery({
    queryKey: ['positions'],
    queryFn: () => api.positions.list(),
  })

  const handleReload = () => {
    queryClient.invalidateQueries({ queryKey: ['positions'] })
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold">保有ポジション</h2>
        <button
          onClick={handleReload}
          className="bg-gray-700 hover:bg-gray-600 text-gray-200 text-sm font-medium px-4 py-2 rounded transition"
        >
          リロード
        </button>
      </div>

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-4 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">ペア</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">取引所</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">方向</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">エントリー価格</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">数量</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">SL</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">TP</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">エントリー日時</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">口座</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={10} className="px-4 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !positions?.length ? (
                <tr>
                  <td colSpan={10} className="px-4 py-8 text-center text-gray-500">
                    保有ポジションはありません
                  </td>
                </tr>
              ) : (
                positions.map((p) => (
                  <tr
                    key={p.trade_id}
                    className="border-b border-gray-800/50 hover:bg-gray-800/30"
                  >
                    <td className="px-4 py-2">{p.strategy_name}</td>
                    <td className="px-4 py-2">{p.pair}</td>
                    <td className="px-4 py-2 text-gray-300">{p.exchange}</td>
                    <td className="px-4 py-2">
                      <span
                        className={
                          p.direction === 'long'
                            ? 'text-emerald-400'
                            : 'text-red-400'
                        }
                      >
                        {p.direction.toUpperCase()}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(p.entry_price).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {p.quantity ? Number(p.quantity).toLocaleString() : '-'}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(p.stop_loss).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(p.take_profit).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-gray-300">
                      {new Date(p.entry_at).toLocaleString('ja-JP', {
                        month: '2-digit',
                        day: '2-digit',
                        hour: '2-digit',
                        minute: '2-digit',
                      })}
                    </td>
                    <td className="px-4 py-2 text-gray-300">
                      {p.paper_account_name || '-'}
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  )
}
