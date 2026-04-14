import { useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import AccountForm from '../components/AccountForm'
import { RiskBadge, useStrategyRiskLookup } from '../components/RiskBadge'
import type {
  TradingAccount,
  CreateTradingAccount,
  UpdateTradingAccount,
} from '../api/types'

export default function Accounts() {
  const queryClient = useQueryClient()
  const [showForm, setShowForm] = useState(false)
  const [editTarget, setEditTarget] = useState<TradingAccount | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<TradingAccount | null>(null)

  const { data: accounts, isLoading } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })
  const lookupRisk = useStrategyRiskLookup()

  const createMut = useMutation({
    mutationFn: (data: CreateTradingAccount) => api.accounts.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['accounts'] })
      setShowForm(false)
    },
  })

  const updateMut = useMutation({
    mutationFn: ({ id, data }: { id: string; data: UpdateTradingAccount }) =>
      api.accounts.update(id, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['accounts'] })
      setEditTarget(null)
    },
  })

  const deleteMut = useMutation({
    mutationFn: (id: string) => api.accounts.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['accounts'] })
      setDeleteTarget(null)
    },
  })

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-xl font-bold">口座管理</h2>
        <button
          onClick={() => setShowForm(true)}
          className="bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium px-4 py-2 rounded transition"
        >
          + 新規作成
        </button>
      </div>

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-4 py-2 text-left text-gray-400 font-medium">名前</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">種別</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">取引所</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">初期残高</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">現在残高</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">含み損益</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">評価額</th>
                <th className="px-4 py-2 text-right text-gray-400 font-medium">レバレッジ</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">通貨</th>
                <th className="px-4 py-2 text-left text-gray-400 font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={11} className="px-4 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !accounts?.length ? (
                <tr>
                  <td colSpan={11} className="px-4 py-8 text-center text-gray-500">
                    口座がありません
                  </td>
                </tr>
              ) : (
                accounts.map((a) => {
                  const unrealized = a.unrealized_pnl ? Number(a.unrealized_pnl) : 0
                  const initial = Number(a.initial_balance)
                  const evaluated = a.evaluated_balance
                    ? Number(a.evaluated_balance)
                    : Number(a.current_balance)
                  const unrealizedColor =
                    unrealized > 0
                      ? 'text-emerald-400'
                      : unrealized < 0
                        ? 'text-red-400'
                        : 'text-gray-300'
                  // Color the evaluated balance relative to the
                  // initial balance so "am I up or down overall?"
                  // is visible at a glance on the account list.
                  const evaluatedColor =
                    Number.isNaN(evaluated) || Number.isNaN(initial)
                      ? ''
                      : evaluated > initial
                        ? 'text-emerald-400'
                        : evaluated < initial
                          ? 'text-red-400'
                          : ''
                  return (
                  <tr key={a.id} className="border-b border-gray-800/50 hover:bg-gray-800/30">
                    <td className="px-4 py-2 font-medium">{a.name}</td>
                    <td className="px-4 py-2 text-gray-300">
                      {a.account_type === 'live' ? '通常' : 'ペーパー'}
                    </td>
                    <td className="px-4 py-2 text-gray-300">{a.exchange}</td>
                    <td className="px-4 py-2 text-gray-300">
                      <div className="flex items-center gap-2">
                        <RiskBadge riskLevel={lookupRisk(a.strategy)} />
                        <span>{a.strategy}</span>
                      </div>
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(a.initial_balance).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">
                      {Number(a.current_balance).toLocaleString()}
                    </td>
                    <td className={`px-4 py-2 text-right ${unrealizedColor}`}>
                      {unrealized >= 0 ? '+' : ''}
                      {Math.round(unrealized).toLocaleString()}
                    </td>
                    <td className={`px-4 py-2 text-right font-medium ${evaluatedColor}`}>
                      {Math.round(evaluated).toLocaleString()}
                    </td>
                    <td className="px-4 py-2 text-right">{a.leverage}x</td>
                    <td className="px-4 py-2 text-gray-300">{a.currency}</td>
                    <td className="px-4 py-2">
                      <div className="flex gap-2">
                        <button
                          onClick={() => setEditTarget(a)}
                          className="text-blue-400 hover:text-blue-300 text-xs"
                        >
                          編集
                        </button>
                        <button
                          onClick={() => setDeleteTarget(a)}
                          className="text-red-400 hover:text-red-300 text-xs"
                        >
                          削除
                        </button>
                      </div>
                    </td>
                  </tr>
                  )
                })
              )}
            </tbody>
          </table>
        </div>
      </div>

      {showForm && (
        <AccountForm
          onCreate={(data) => createMut.mutate(data)}
          onCancel={() => setShowForm(false)}
        />
      )}

      {editTarget && (
        <AccountForm
          key={editTarget.id}
          account={editTarget}
          onUpdate={(data) =>
            updateMut.mutate({ id: editTarget.id, data })
          }
          onCancel={() => setEditTarget(null)}
        />
      )}

      {deleteTarget && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
          <div className="bg-gray-900 rounded-lg p-6 w-full max-w-sm shadow-xl">
            <h3 className="text-lg font-bold mb-2">口座を削除</h3>
            <p className="text-gray-400 text-sm mb-4">
              「{deleteTarget.name}」を削除しますか？この操作は取り消せません。
            </p>
            <div className="flex gap-3">
              <button
                onClick={() => deleteMut.mutate(deleteTarget.id)}
                className="flex-1 bg-red-600 hover:bg-red-700 text-white text-sm font-medium py-2 rounded transition"
              >
                削除
              </button>
              <button
                onClick={() => setDeleteTarget(null)}
                className="flex-1 bg-gray-700 hover:bg-gray-600 text-gray-200 text-sm font-medium py-2 rounded transition"
              >
                キャンセル
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
