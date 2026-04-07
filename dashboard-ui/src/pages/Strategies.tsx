import { RiskBadge, useStrategyCatalogQuery } from '../components/RiskBadge'
import type { Strategy } from '../api/types'

const categoryLabel: Record<Strategy['category'], string> = {
  fx: 'FX',
  crypto: '暗号資産',
}

const categoryBadgeClass: Record<Strategy['category'], string> = {
  fx: 'bg-blue-900/40 text-blue-300 border border-blue-800',
  crypto: 'bg-amber-900/40 text-amber-300 border border-amber-800',
}

/**
 * Read-only strategy catalog. The data comes from the `strategies` table
 * which is metadata only — the trading engine still loads strategies from
 * `config/default.toml`. Adding a new strategy requires both a Rust impl
 * and a migration row.
 */
export default function Strategies() {
  const { data, isLoading, error } = useStrategyCatalogQuery()

  if (isLoading) {
    return <div className="text-gray-400">読み込み中…</div>
  }
  if (error) {
    return (
      <div className="text-red-400">
        戦略一覧の取得に失敗しました: {String(error)}
      </div>
    )
  }
  if (!data || data.length === 0) {
    return <div className="text-gray-400">戦略が登録されていません。</div>
  }

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-xl font-bold">戦略一覧</h2>
        <p className="text-sm text-gray-400 mt-1">
          コードで実装され、DB に登録されている戦略の一覧です。実際の有効
          / 無効・パラメータは <code>config/default.toml</code>{' '}
          で管理されています。
        </p>
      </div>
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        {data.map((s) => (
          <StrategyCard key={s.name} strategy={s} />
        ))}
      </div>
    </div>
  )
}

function StrategyCard({ strategy }: { strategy: Strategy }) {
  return (
    <div className="bg-gray-900 border border-gray-800 rounded-lg p-5 space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-base font-semibold text-gray-100">
            {strategy.display_name}
          </h3>
          <code className="text-xs text-gray-500">{strategy.name}</code>
        </div>
        <div className="flex flex-col items-end gap-1">
          <RiskBadge riskLevel={strategy.risk_level} />
          <span
            className={`text-xs px-2 py-0.5 rounded ${categoryBadgeClass[strategy.category]}`}
          >
            {categoryLabel[strategy.category]}
          </span>
        </div>
      </div>
      <p className="text-sm text-gray-300">{strategy.description}</p>
      <details className="group">
        <summary className="text-xs text-gray-400 cursor-pointer hover:text-gray-200 select-none">
          アルゴリズム詳細を表示
        </summary>
        <pre className="mt-2 whitespace-pre-wrap text-xs text-gray-300 bg-gray-950 border border-gray-800 rounded p-3 leading-relaxed">
          {strategy.algorithm.trim()}
        </pre>
        {Object.keys(strategy.default_params).length > 0 && (
          <div className="mt-2">
            <div className="text-xs text-gray-500 mb-1">既定パラメータ:</div>
            <pre className="text-xs text-gray-300 bg-gray-950 border border-gray-800 rounded p-3">
              {JSON.stringify(strategy.default_params, null, 2)}
            </pre>
          </div>
        )}
      </details>
    </div>
  )
}
