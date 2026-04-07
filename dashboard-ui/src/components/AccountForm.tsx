import { useState, useMemo, useEffect } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type {
  CreatePaperAccount,
  PaperAccount,
  UpdatePaperAccount,
} from '../api/types'

interface Props {
  account?: PaperAccount | null
  onCreate?: (data: CreatePaperAccount) => void
  onUpdate?: (data: UpdatePaperAccount) => void
  onCancel: () => void
}

export default function AccountForm({
  account,
  onCreate,
  onUpdate,
  onCancel,
}: Props) {
  const isEdit = !!account
  const [name, setName] = useState(account?.name ?? '')
  const [exchange, setExchange] = useState(account?.exchange ?? 'bitflyer_cfd')
  const [initialBalance, setInitialBalance] = useState(
    account?.initial_balance ?? '',
  )
  const [leverage, setLeverage] = useState(account?.leverage ?? '1')
  const [strategy, setStrategy] = useState(account?.strategy ?? '')
  const [currency, setCurrency] = useState(account?.currency ?? 'JPY')

  // Strategy catalog (DB-managed). Filtered by exchange so the dropdown only
  // shows strategies compatible with the selected exchange. The catalog is
  // metadata only — the runtime engine still loads strategies from
  // config/default.toml — but using a select prevents typos and gives the
  // user a list of valid choices.
  const exchangeCategory: 'fx' | 'crypto' =
    exchange === 'oanda' ? 'fx' : 'crypto'
  const strategiesQuery = useQuery({
    queryKey: ['strategies', exchangeCategory],
    queryFn: () => api.strategies.list(exchangeCategory),
  })
  const strategyOptions = useMemo(
    () => strategiesQuery.data ?? [],
    [strategiesQuery.data],
  )

  // When the user switches exchange in create mode, the previously selected
  // strategy (e.g. crypto_trend_v1) may no longer exist for the new
  // category. Clear it so the dropdown forces a re-pick instead of
  // submitting an out-of-category value. Edit mode preserves the original
  // exchange (the field is disabled), so this only fires for new accounts.
  useEffect(() => {
    if (isEdit) return
    if (!strategy) return
    if (strategiesQuery.isLoading) return
    if (!strategyOptions.some((s) => s.name === strategy)) {
      setStrategy('')
    }
  }, [isEdit, strategy, strategyOptions, strategiesQuery.isLoading])
  const [accountType, setAccountType] = useState<'paper' | 'live'>(
    (account?.account_type as 'paper' | 'live') ?? 'paper',
  )

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (isEdit) {
      // Edit mode: only mutable fields. Backend rejects unknown fields.
      onUpdate?.({
        name,
        leverage,
        strategy,
      })
    } else {
      onCreate?.({
        name,
        exchange,
        initial_balance: initialBalance,
        leverage,
        strategy,
        account_type: accountType,
        currency: currency || undefined,
      })
    }
  }

  const inputClass =
    'w-full bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-2 focus:outline-none focus:border-blue-500'
  const disabledClass = 'opacity-50 cursor-not-allowed'
  const labelClass = 'block text-sm text-gray-400 mb-1'

  return (
    <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
      <div className="bg-gray-900 rounded-lg p-6 w-full max-w-md shadow-xl">
        <h3 className="text-lg font-bold mb-4">
          {isEdit ? '口座を編集' : '口座を作成'}
        </h3>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label className={labelClass}>口座名</label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
              className={inputClass}
              placeholder="例: BTC検証用"
            />
          </div>
          <div>
            <label className={labelClass}>種別</label>
            <div className="flex gap-4 text-sm">
              <label className={`flex items-center gap-2 ${isEdit ? disabledClass : ''}`}>
                <input
                  type="radio"
                  name="account_type"
                  value="paper"
                  checked={accountType === 'paper'}
                  onChange={() => setAccountType('paper')}
                  disabled={isEdit}
                />
                ペーパー
              </label>
              <label className={`flex items-center gap-2 ${isEdit ? disabledClass : ''}`}>
                <input
                  type="radio"
                  name="account_type"
                  value="live"
                  checked={accountType === 'live'}
                  onChange={() => setAccountType('live')}
                  disabled={isEdit}
                />
                通常
              </label>
            </div>
          </div>
          <div>
            <label className={labelClass}>取引所</label>
            <select
              value={exchange}
              onChange={(e) => setExchange(e.target.value)}
              disabled={isEdit}
              className={`${inputClass} ${isEdit ? disabledClass : ''}`}
            >
              <option value="bitflyer_cfd">Crypto (bitFlyer)</option>
              <option value="oanda">FX (OANDA)</option>
            </select>
          </div>
          <div>
            <label className={labelClass}>初期残高</label>
            <input
              type="number"
              value={initialBalance}
              onChange={(e) => setInitialBalance(e.target.value)}
              required
              min="0"
              step="any"
              disabled={isEdit}
              className={`${inputClass} ${isEdit ? disabledClass : ''}`}
              placeholder="1000000"
            />
          </div>
          <div>
            <label className={labelClass}>レバレッジ</label>
            <input
              type="number"
              value={leverage}
              onChange={(e) => setLeverage(e.target.value)}
              required
              min="1"
              className={inputClass}
            />
          </div>
          <div>
            <label className={labelClass}>戦略</label>
            <select
              value={strategy}
              onChange={(e) => setStrategy(e.target.value)}
              required
              className={inputClass}
              disabled={strategiesQuery.isLoading}
            >
              <option value="" disabled>
                {strategiesQuery.isLoading
                  ? '読み込み中…'
                  : '戦略を選択してください'}
              </option>
              {strategyOptions.map((s) => {
                const riskTag =
                  s.risk_level === 'low'
                    ? '[慎重]'
                    : s.risk_level === 'high'
                      ? '[攻め]'
                      : '[標準]'
                return (
                  <option key={s.name} value={s.name}>
                    {riskTag} {s.display_name} ({s.name})
                  </option>
                )
              })}
              {/* Edit-mode safety: if the existing account references a
                  strategy that has since been removed from the catalog,
                  still show it so the user can re-select something else. */}
              {isEdit &&
                strategy &&
                !strategyOptions.some((s) => s.name === strategy) && (
                  <option value={strategy}>{strategy} (未登録)</option>
                )}
            </select>
          </div>
          <div>
            <label className={labelClass}>通貨</label>
            <select
              value={currency}
              onChange={(e) => setCurrency(e.target.value)}
              disabled={isEdit}
              className={`${inputClass} ${isEdit ? disabledClass : ''}`}
            >
              <option value="JPY">JPY</option>
              <option value="USD">USD</option>
              <option value="USDT">USDT</option>
            </select>
          </div>
          <div className="flex gap-3 pt-2">
            <button
              type="submit"
              className="flex-1 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium py-2 rounded transition"
            >
              {isEdit ? '更新' : '作成'}
            </button>
            <button
              type="button"
              onClick={onCancel}
              className="flex-1 bg-gray-700 hover:bg-gray-600 text-gray-200 text-sm font-medium py-2 rounded transition"
            >
              キャンセル
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
