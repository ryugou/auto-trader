import { useEffect, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  useReactTable,
  getCoreRowModel,
  createColumnHelper,
  flexRender,
} from '@tanstack/react-table'
import { api } from '../api/client'
import type { TradeRow, TradeEvent } from '../api/types'

interface TradeTableProps {
  filters: {
    exchange?: string
    paper_account_id?: string
    from?: string
    to?: string
  }
}

const col = createColumnHelper<TradeRow>()

function formatDate(iso: string | null): string {
  if (!iso) return '-'
  return new Date(iso).toLocaleString('ja-JP', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function formatNum(value: string | null): string {
  if (!value) return '-'
  return Number(value).toLocaleString()
}

function holdingTime(entry: string, exit: string | null): string {
  if (!exit) return '-'
  const ms = new Date(exit).getTime() - new Date(entry).getTime()
  const minutes = Math.floor(ms / 60_000)
  if (minutes < 60) return `${minutes}m`
  const hours = Math.floor(minutes / 60)
  const mins = minutes % 60
  if (hours < 24) return `${hours}h${mins > 0 ? `${mins}m` : ''}`
  const days = Math.floor(hours / 24)
  return `${days}d${hours % 24}h`
}

function buildColumns(
  accountMap: Map<string, string>,
  expanded: Set<string>,
  toggle: (id: string) => void,
) {
  return [
    col.display({
      id: 'expander',
      header: '',
      cell: (info) => {
        const id = info.row.original.id
        const isOpen = expanded.has(id)
        return (
          <button
            type="button"
            onClick={(e) => {
              // Stop the click from bubbling up to the row's onClick — we
              // want the chevron to be the only place the toggle happens
              // even though the whole row is also clickable.
              e.stopPropagation()
              toggle(id)
            }}
            aria-label={isOpen ? '閉じる' : '開く'}
            className="text-gray-400 hover:text-gray-200 transition-transform"
            style={{ transform: isOpen ? 'rotate(90deg)' : 'rotate(0deg)' }}
          >
            {/* Right-pointing chevron; CSS rotates it 90° when expanded
                so the same icon is reused for both states. */}
            <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
              <path d="M4 2 L8 6 L4 10 Z" />
            </svg>
          </button>
        )
      },
    }),
    col.accessor('entry_at', {
      header: '日時',
      cell: (info) => formatDate(info.getValue()),
    }),
    col.accessor('strategy_name', { header: '戦略' }),
    col.accessor('paper_account_id', {
      header: '口座',
      cell: (info) => {
        const id = info.getValue()
        if (!id) return '-'
        return accountMap.get(id) ?? '-'
      },
    }),
    col.accessor('account_type', {
      header: '種別',
      cell: (info) => {
        const t = info.getValue()
        if (t === 'paper') return 'ペーパー'
        if (t === 'live') return '通常'
        return '-'
      },
    }),
    col.accessor('pair', { header: 'ペア' }),
    col.accessor('direction', {
      header: '方向',
      cell: (info) => {
        const dir = info.getValue()
        return (
          <span className={dir === 'long' ? 'text-emerald-400' : 'text-red-400'}>
            {dir.toUpperCase()}
          </span>
        )
      },
    }),
    col.accessor('entry_price', {
      header: 'エントリー',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.accessor('exit_price', {
      header: 'エグジット',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.accessor('quantity', {
      header: '数量',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.accessor('pnl_amount', {
      header: 'PnL',
      cell: (info) => {
        const val = info.getValue()
        if (!val) return '-'
        const n = Number(val)
        return (
          <span className={n >= 0 ? 'text-emerald-400' : 'text-red-400'}>
            {n >= 0 ? '+' : ''}{Math.round(n).toLocaleString()}
          </span>
        )
      },
    }),
    col.accessor('fees', {
      header: '手数料',
      cell: (info) => formatNum(info.getValue()),
    }),
    col.display({
      id: 'net_pnl',
      header: 'Net PnL',
      cell: (info) => {
        const row = info.row.original
        if (!row.pnl_amount) return '-'
        const net = Number(row.pnl_amount) - Number(row.fees)
        return (
          <span className={net >= 0 ? 'text-emerald-400' : 'text-red-400'}>
            {net >= 0 ? '+' : ''}{Math.round(net).toLocaleString()}
          </span>
        )
      },
    }),
    col.display({
      id: 'holding_time',
      header: '保有時間',
      cell: (info) => {
        const row = info.row.original
        return holdingTime(row.entry_at, row.exit_at)
      },
    }),
  ]
}

export default function TradeTable({ filters }: TradeTableProps) {
  const [page, setPage] = useState(1)
  const perPage = 20
  // IDs of trades whose timeline is expanded. Plain Set instead of
  // TanStack's expanded-row API because each child needs its own
  // independent fetch and we want to avoid carrying the lazy-load
  // state through the table model.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  useEffect(() => {
    setPage(1)
    // Reset expansion when filters change so a stale row id doesn't
    // briefly show up over the new result set.
    setExpanded(new Set())
  }, [filters])

  const toggleExpanded = (id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const { data: accounts } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const accountMap = useMemo(() => {
    const m = new Map<string, string>()
    accounts?.forEach((a) => m.set(a.id, a.name))
    return m
  }, [accounts])

  const columns = useMemo(
    () => buildColumns(accountMap, expanded, toggleExpanded),
    [accountMap, expanded],
  )

  const { data, isLoading } = useQuery({
    queryKey: ['trades', filters, page],
    queryFn: () =>
      api.trades.list({
        ...filters,
        page: String(page),
        per_page: String(perPage),
      }),
  })

  // eslint-disable-next-line react-hooks/incompatible-library
  const table = useReactTable({
    data: data?.trades ?? [],
    columns,
    getCoreRowModel: getCoreRowModel(),
  })

  const totalPages = data ? Math.ceil(data.total / perPage) : 0

  return (
    <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            {table.getHeaderGroups().map((hg) => (
              <tr key={hg.id} className="border-b border-gray-800">
                {hg.headers.map((h) => (
                  <th
                    key={h.id}
                    className="px-3 py-2 text-left text-gray-400 font-medium whitespace-nowrap"
                  >
                    {flexRender(h.column.columnDef.header, h.getContext())}
                  </th>
                ))}
              </tr>
            ))}
          </thead>
          <tbody>
            {isLoading ? (
              <tr>
                <td colSpan={columns.length} className="px-3 py-8 text-center text-gray-500">
                  読み込み中...
                </td>
              </tr>
            ) : table.getRowModel().rows.length === 0 ? (
              <tr>
                <td colSpan={columns.length} className="px-3 py-8 text-center text-gray-500">
                  トレードデータがありません
                </td>
              </tr>
            ) : (
              table.getRowModel().rows.flatMap((row) => {
                const tradeId = row.original.id
                const isExpanded = expanded.has(tradeId)
                const rows = [
                  <tr
                    key={row.id}
                    className="border-b border-gray-800/50 hover:bg-gray-800/30 cursor-pointer"
                    onClick={() => toggleExpanded(tradeId)}
                  >
                    {row.getVisibleCells().map((cell) => (
                      <td key={cell.id} className="px-3 py-2 whitespace-nowrap">
                        {flexRender(cell.column.columnDef.cell, cell.getContext())}
                      </td>
                    ))}
                  </tr>,
                ]
                if (isExpanded) {
                  rows.push(
                    <tr key={`${row.id}-events`} className="bg-gray-950/50">
                      <td colSpan={columns.length} className="px-0 py-0">
                        <TradeEventTimeline tradeId={tradeId} />
                      </td>
                    </tr>,
                  )
                }
                return rows
              })
            )}
          </tbody>
        </table>
      </div>

      {/* End of table body */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between px-4 py-3 border-t border-gray-800">
          <span className="text-sm text-gray-400">
            {data?.total ?? 0} 件中 {(page - 1) * perPage + 1}-
            {Math.min(page * perPage, data?.total ?? 0)} 件
          </span>
          <div className="flex gap-2">
            <button
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page <= 1}
              className="px-3 py-1 text-sm bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              前へ
            </button>
            <button
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
              disabled={page >= totalPages}
              className="px-3 py-1 text-sm bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              次へ
            </button>
          </div>
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Expandable trade timeline (OPEN → overnight fees → CLOSE)
// ---------------------------------------------------------------------------

function eventLabel(kind: TradeEvent['kind']): string {
  switch (kind) {
    case 'open':
      return 'OPEN'
    case 'close':
      return 'CLOSE'
    case 'overnight_fee':
      return 'overnight'
  }
}

function eventColor(kind: TradeEvent['kind']): string {
  switch (kind) {
    case 'open':
      return 'text-sky-400'
    case 'close':
      return 'text-amber-400'
    case 'overnight_fee':
      return 'text-gray-500'
  }
}

function formatSignedYen(raw: string | null): string {
  if (raw == null) return '-'
  const n = Number(raw)
  if (Number.isNaN(n)) return raw
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

/// Render a cash-delta cell. The backend deliberately returns `null`
/// for OPEN/CLOSE rows on trades that predate the margin-lock contract
/// (the ledger has no margin event for them, so we cannot reconstruct
/// the lock/refund leg). Conflating "null" with "0/-" hides this
/// distinction; render an explicit `不明` label instead so the user
/// knows the data is missing rather than zero.
function renderCashDelta(ev: TradeEvent) {
  if (ev.cash_delta == null) {
    if (ev.kind === 'overnight_fee') {
      return <span className="text-gray-500">-</span>
    }
    return (
      <span
        className="text-gray-500 italic"
        title="このトレードには margin lock / release のレジャーが無いため、口座への正確な現金影響を再構築できません"
      >
        不明
      </span>
    )
  }
  const n = Number(ev.cash_delta)
  return (
    <span
      className={`font-mono ${n >= 0 ? 'text-emerald-400' : 'text-red-400'}`}
    >
      {formatSignedYen(ev.cash_delta)}
    </span>
  )
}

function TradeEventTimeline({ tradeId }: { tradeId: string }) {
  const { data, isLoading, isError } = useQuery({
    queryKey: ['trade-events', tradeId],
    queryFn: () => api.trades.events(tradeId),
    // Closed-trade timelines never change after the CLOSE event lands.
    // 1 minute is plenty to keep open trades' fee accruals reasonably
    // fresh while still saving the network round-trip on rapid
    // open/close toggling.
    staleTime: 60_000,
  })

  if (isLoading) {
    return (
      <div className="px-12 py-3 text-xs text-gray-500">タイムライン読み込み中...</div>
    )
  }
  if (isError || !data) {
    return (
      <div className="px-12 py-3 text-xs text-red-400">
        タイムラインの取得に失敗しました
      </div>
    )
  }
  return (
    <div className="px-12 py-2 border-l-2 border-gray-800">
      <table className="w-full text-xs">
        <thead>
          <tr className="text-gray-500">
            <th className="text-left font-normal py-1 w-24">種別</th>
            <th className="text-left font-normal py-1 w-32">日時</th>
            <th className="text-right font-normal py-1 w-32">価格</th>
            <th className="text-right font-normal py-1 w-24">数量</th>
            <th className="text-right font-normal py-1 w-32">残高変動</th>
            <th className="text-right font-normal py-1 w-24">PnL</th>
          </tr>
        </thead>
        <tbody>
          {data.events.map((ev) => (
            // Composite key: kind + timestamp uniquely identifies an
            // event within a single trade's timeline (a trade can only
            // OPEN/CLOSE once, and overnight fees can only fire once
            // per UTC midnight). Index keys would be unstable across
            // refetches that grow the timeline (open trade accruing a
            // new overnight fee).
            <tr key={`${ev.kind}:${ev.occurred_at}`} className="border-t border-gray-800/40">
              <td className={`py-1 font-mono ${eventColor(ev.kind)}`}>
                {eventLabel(ev.kind)}
              </td>
              <td className="py-1 text-gray-300">{formatDate(ev.occurred_at)}</td>
              <td className="py-1 text-right text-gray-300">
                {formatNum(ev.price)}
              </td>
              <td className="py-1 text-right text-gray-300">
                {formatNum(ev.quantity)}
              </td>
              <td className="py-1 text-right">{renderCashDelta(ev)}</td>
              <td
                className={`py-1 text-right font-mono ${
                  ev.pnl_amount == null
                    ? 'text-gray-500'
                    : Number(ev.pnl_amount) >= 0
                      ? 'text-emerald-400'
                      : 'text-red-400'
                }`}
              >
                {formatSignedYen(ev.pnl_amount)}
              </td>
            </tr>
          ))}
          {/* Open trades have no CLOSE row yet — make that explicit so
              the user understands the timeline is intentionally short. */}
          {data.events.every((e) => e.kind !== 'close') && (
            <tr className="border-t border-gray-800/40">
              <td colSpan={6} className="py-1 text-center text-gray-500 italic">
                保有中
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  )
}
