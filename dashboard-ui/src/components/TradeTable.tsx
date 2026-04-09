import { useCallback, useEffect, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  useReactTable,
  getCoreRowModel,
  createColumnHelper,
  flexRender,
} from '@tanstack/react-table'
import { api } from '../api/client'
import type { PaperAccount, TradeRow, TradeEvent } from '../api/types'

interface TradeTableProps {
  account: PaperAccount
  from?: string
  to?: string
}

const col = createColumnHelper<TradeRow>()

function formatDate(iso: string | null): string {
  if (!iso) return '-'
  // Pin display to JST regardless of the viewer's local OS timezone.
  // The trader runs on JST schedules (bitFlyer overnight fees fire at
  // 04:30 JST etc.), so timestamps need to read as JST or they become
  // confusing when accessing the dashboard from a non-JST machine.
  return new Date(iso).toLocaleString('ja-JP', {
    timeZone: 'Asia/Tokyo',
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

// Integer-only display used across the main trade table rows. The
// trader never books sub-unit amounts for crypto/FX at the
// reporting layer, so dropping the decimals is just noise removal.
function formatInt(value: string | null): string {
  if (!value) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return '-'
  return Math.round(n).toLocaleString()
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

// Render a balance with the appropriate currency glyph.
// - JPY rounds to integer (the trader never books sub-yen amounts).
// - USD pins to two decimals (cents are meaningful for FX P&L).
// - Other currencies fall through with the locale default and a
//   trailing currency code so the value is still readable instead
//   of silently hidden. We do not impose 2-decimal pinning on
//   unknown currencies because their conventional precision varies
//   (e.g. JPY-like minor-unit-less currencies should not gain
//   spurious decimals).
function formatBalance(amount: string | undefined, currency: string): string {
  if (amount == null) return '-'
  const n = Number(amount)
  if (Number.isNaN(n)) return '-'
  if (currency === 'JPY') {
    return `¥${Math.round(n).toLocaleString()}`
  }
  if (currency === 'USD') {
    return `$${n.toLocaleString(undefined, {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    })}`
  }
  return `${n.toLocaleString()} ${currency}`
}

// Strip trailing zeros from the leverage value (stored as a decimal
// string like "5.00") so the header reads "5x" instead of "5.00x".
function formatLeverage(leverage: string): string {
  const n = Number(leverage)
  if (Number.isNaN(n)) return `${leverage}x`
  return `${n}x`
}

function buildColumns(
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
      cell: (info) => formatInt(info.getValue()),
    }),
    col.accessor('exit_price', {
      header: 'エグジット',
      cell: (info) => formatInt(info.getValue()),
    }),
    col.accessor('quantity', {
      header: '数量',
      cell: (info) => formatInt(info.getValue()),
    }),
    col.accessor('fees', {
      header: '手数料',
      cell: (info) => formatInt(info.getValue()),
    }),
    col.display({
      id: 'net_pnl',
      header: '純損益',
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

const PER_PAGE = 10

export default function TradeTable({ account, from, to }: TradeTableProps) {
  // NOTE on filter changes: this component is intentionally remounted
  // by its parent (`Trades.tsx` keys it on `account.id + from + to`)
  // when the period filter changes, so the local `page` and
  // `expanded` state are reset for free without an extra render +
  // wasted query. We deliberately do not run a `useEffect([from,to])`
  // here for that reason.
  const [page, setPage] = useState(1)
  // IDs of trades whose timeline is expanded. Plain Set instead of
  // TanStack's expanded-row API because each child needs its own
  // independent fetch and we want to avoid carrying the lazy-load
  // state through the table model.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  const toggleExpanded = useCallback((id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }, [])

  const columns = useMemo(
    () => buildColumns(expanded, toggleExpanded),
    [expanded, toggleExpanded],
  )

  const { data, isLoading, isError } = useQuery({
    queryKey: ['trades', { accountId: account.id, from, to, page }],
    queryFn: () =>
      api.trades.list({
        paper_account_id: account.id,
        from,
        to,
        page: String(page),
        per_page: String(PER_PAGE),
      }),
  })

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PER_PAGE))
  const rangeStart = total === 0 ? 0 : (page - 1) * PER_PAGE + 1
  const rangeEnd = Math.min(page * PER_PAGE, total)

  // Clamp page back into range if the underlying total shrinks (e.g.
  // a refetch returns fewer rows than the page we were viewing). The
  // refetch would otherwise leave us showing "no data" on a page
  // that no longer exists. Only clamp once `data` has actually
  // arrived so we don't fight the initial loading state.
  useEffect(() => {
    if (data && page > totalPages) {
      setPage(totalPages)
      setExpanded(new Set())
    }
  }, [data, page, totalPages])

  // Going to a new page is conceptually the same as switching the
  // data window — drop any open timelines so a stale row id from the
  // previous page cannot leak into the new render.
  const goToPage = (next: number) => {
    setPage(next)
    setExpanded(new Set())
  }

  // eslint-disable-next-line react-hooks/incompatible-library
  const table = useReactTable({
    data: data?.trades ?? [],
    columns,
    getCoreRowModel: getCoreRowModel(),
  })

  // Match Accounts.tsx convention: `live` is the explicit "通常"
  // branch, anything else (including unknown values) renders as
  // ペーパー. This biases the badge toward "paper" for any
  // unrecognised account_type so a misconfigured account does not
  // accidentally look like a live one.
  const isLive = account.account_type === 'live'
  const typeBadge = isLive ? '通常' : 'ペーパー'
  const typeBadgeClass = isLive
    ? 'bg-sky-900 text-sky-200'
    : 'bg-gray-700 text-gray-200'

  return (
    <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-2 px-4 py-3 border-b border-gray-800">
        <div className="flex items-center gap-2">
          <h3 className="text-base font-semibold text-gray-100">{account.name}</h3>
          <span
            className={`text-xs font-medium px-2 py-0.5 rounded ${typeBadgeClass}`}
          >
            {typeBadge}
          </span>
        </div>
        <div className="text-xs text-gray-400 flex flex-wrap gap-x-3 gap-y-1">
          {account.evaluated_balance != null && (() => {
            // Color the evaluated balance relative to the initial
            // balance so at-a-glance you can see if the account is
            // up or down overall. Use the same green/red scheme as
            // other +/- indicators across the dashboard.
            const evaluated = Number(account.evaluated_balance)
            const initial = Number(account.initial_balance)
            const balanceClass = Number.isNaN(evaluated) || Number.isNaN(initial)
              ? 'text-gray-100'
              : evaluated > initial
                ? 'text-emerald-400'
                : evaluated < initial
                  ? 'text-red-400'
                  : 'text-gray-100'
            return (
              <span>
                評価額{' '}
                <span className={`${balanceClass} font-mono`}>
                  {formatBalance(account.evaluated_balance, account.currency)}
                </span>
              </span>
            )
          })()}
          <span>
            残高{' '}
            <span className="text-gray-100 font-mono">
              {formatBalance(account.current_balance, account.currency)}
            </span>
          </span>
          <span>
            レバレッジ{' '}
            <span className="text-gray-100 font-mono">
              {formatLeverage(account.leverage)}
            </span>
          </span>
        </div>
      </div>

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
            ) : isError ? (
              <tr>
                <td colSpan={columns.length} className="px-3 py-8 text-center text-red-400">
                  トレード履歴の取得に失敗しました
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

      {/* Per-table prev/next pager. Hidden when there is only one
          page of data so empty / very-quiet accounts don't carry a
          purely decorative footer. */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between gap-2 px-4 py-2 border-t border-gray-800 text-xs text-gray-400">
          <span>
            {total} 件中 {rangeStart}-{rangeEnd} 件
          </span>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => goToPage(Math.max(1, page - 1))}
              disabled={page <= 1}
              className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              前へ
            </button>
            <button
              type="button"
              onClick={() => goToPage(Math.min(totalPages, page + 1))}
              disabled={page >= totalPages}
              className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
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
