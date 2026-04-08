# Trades Per-Account Tables — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the Trades page into one table per paper account, grouped by exchange (crypto first, then FX), with period filter only.

**Architecture:** `Trades.tsx` orchestrates: fetches accounts, groups by exchange, renders one `<TradeTable>` per account. `TradeTable` is refactored from "filter-driven paginated table" into "single-account latest-10 table" that takes a `PaperAccount` directly and renders its own header (name / type badge / balances / leverage).

**Tech Stack:** React 19, TanStack Query v5, TanStack Table v8, Tailwind v4 (no frontend test framework — verification via `npm run lint` + `npm run build` + manual smoke test).

**Spec:** `docs/superpowers/specs/2026-04-08-trades-per-account-design.md`

---

> **Note (post-implementation):** This plan was authored before the
> per-account prev/next pagination requirement was added. The
> implementation that landed (commits `e6b1c07` and `2bcdb24`)
> includes a small per-table page state with prev/next buttons,
> page-aware query keys, and a clamp effect for total-shrink. See
> the spec for the authoritative behaviour.

## File Structure

- **Modify** `dashboard-ui/src/components/TradeTable.tsx`
  - Change props from `{ filters }` to `{ account: PaperAccount; from?: string; to?: string }`
  - Replace global pagination with per-table prev/next paging (10 rows / page)
  - Remove `口座` and `種別` columns
  - Add per-table header: name + type badge + evaluated balance / current balance / leverage
  - Fetch trades for the given account, page by page
- **Modify** `dashboard-ui/src/pages/Trades.tsx`
  - Replace `PageFilters` with a period-only filter (inline)
  - Fetch accounts via `api.accounts.list`
  - Group by exchange class (`crypto` / `fx` / `other`) in that order
  - Sort within each group by account name
  - Render `<TradeTable>` per account, keyed on `account.id + from + to`
    so a period change remounts the child (drops local page/expanded
    state without an extra render); show empty state if no accounts

No new files. `PageFilters.tsx` is left untouched (still used by Positions, etc).

---

## Task 1: Refactor TradeTable to single-account mode

**Files:**
- Modify: `dashboard-ui/src/components/TradeTable.tsx`

- [ ] **Step 1: Replace the full contents of `dashboard-ui/src/components/TradeTable.tsx`**

The file currently exports `TradeTable` as a paginated multi-account table. Replace it with a single-account version. Keep the existing `TradeEventTimeline` component and all its helpers (`eventLabel`, `eventColor`, `formatSignedYen`, `renderCashDelta`) **unchanged** — only the top portion of the file (imports, columns, main component) is rewritten.

Write the file as:

```tsx
import { useMemo, useState } from 'react'
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

// Render a balance with the appropriate currency glyph. JPY rounds to
// integer (the trader never books sub-yen amounts); everything else
// keeps two decimals. Unknown currencies fall back to a trailing code
// so the value is still readable instead of silently hidden.
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

const PER_PAGE = 10

export default function TradeTable({ account, from, to }: TradeTableProps) {
  // IDs of trades whose timeline is expanded. Plain Set instead of
  // TanStack's expanded-row API because each child needs its own
  // independent fetch and we want to avoid carrying the lazy-load
  // state through the table model.
  const [expanded, setExpanded] = useState<Set<string>>(new Set())

  const toggleExpanded = (id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  const columns = useMemo(
    () => buildColumns(expanded, toggleExpanded),
    [expanded],
  )

  const { data, isLoading } = useQuery({
    queryKey: ['trades', { accountId: account.id, from, to }],
    queryFn: () =>
      api.trades.list({
        paper_account_id: account.id,
        from,
        to,
        page: '1',
        per_page: String(PER_PAGE),
      }),
  })

  // eslint-disable-next-line react-hooks/incompatible-library
  const table = useReactTable({
    data: data?.trades ?? [],
    columns,
    getCoreRowModel: getCoreRowModel(),
  })

  const isPaper = account.account_type === 'paper'
  const typeBadge = isPaper ? 'ペーパー' : '通常'
  const typeBadgeClass = isPaper
    ? 'bg-gray-700 text-gray-200'
    : 'bg-sky-900 text-sky-200'

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
          {account.evaluated_balance != null && (
            <span>
              評価額{' '}
              <span className="text-gray-100 font-mono">
                {formatBalance(account.evaluated_balance, account.currency)}
              </span>
            </span>
          )}
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
```

- [ ] **Step 2: Run lint to catch type/style issues in the new file**

Run: `cd dashboard-ui && npm run lint`
Expected: PASS (no errors). Warnings allowed only if they were already present on `main`.

- [ ] **Step 3: Run build to verify TypeScript compiles**

Run: `cd dashboard-ui && npm run build`
Expected: PASS. Note: callers of `TradeTable` will break at this point (Trades.tsx still passes `filters`) — Task 2 fixes that. If the build fails **only** on `Trades.tsx` in the `TradeTable` props mismatch, proceed to Task 2 without committing yet.

- [ ] **Step 4: Stage (do not commit yet)**

This task intentionally leaves the tree in a broken state that Task 2 repairs. Run:

```bash
git add dashboard-ui/src/components/TradeTable.tsx
```

---

## Task 2: Rewrite Trades page with per-account grouping

**Files:**
- Modify: `dashboard-ui/src/pages/Trades.tsx`

- [ ] **Step 1: Replace the full contents of `dashboard-ui/src/pages/Trades.tsx`**

```tsx
import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import TradeTable from '../components/TradeTable'
import type { PaperAccount } from '../api/types'

const JST_OFFSET_MS = 9 * 60 * 60 * 1000

// JST (UTC+9) 基準の YYYY-MM-DD を返す
function jstDateString(date: Date): string {
  return new Date(date.getTime() + JST_OFFSET_MS).toISOString().slice(0, 10)
}

function periodToRange(period: string): { from?: string; to?: string } {
  if (!period) return {}
  const now = new Date()
  const to = jstDateString(now)
  if (period === 'today') return { from: to, to }
  if (period === '1w') {
    const d = new Date(now)
    d.setUTCDate(d.getUTCDate() - 7)
    return { from: jstDateString(d), to }
  }
  if (period === '1m') {
    const d = new Date(now)
    d.setUTCMonth(d.getUTCMonth() - 1)
    return { from: jstDateString(d), to }
  }
  return {}
}

type ExchangeGroupKey = 'crypto' | 'fx' | 'other'

// Classify an exchange string into our two visual groups. Unknown
// exchanges fall into `other` so that a typo or a newly added venue
// still renders somewhere instead of being silently dropped.
function exchangeGroup(exchange: string): ExchangeGroupKey {
  if (exchange.startsWith('bitflyer')) return 'crypto'
  if (exchange === 'oanda') return 'fx'
  return 'other'
}

interface AccountGroup {
  key: ExchangeGroupKey
  label: string
  accounts: PaperAccount[]
}

// Build the ordered group list. Crypto first, then FX, then anything
// unclassified (so we notice during manual QA).
function buildGroups(accounts: PaperAccount[]): AccountGroup[] {
  const crypto: PaperAccount[] = []
  const fx: PaperAccount[] = []
  const other: PaperAccount[] = []
  for (const a of accounts) {
    const g = exchangeGroup(a.exchange)
    if (g === 'crypto') crypto.push(a)
    else if (g === 'fx') fx.push(a)
    else other.push(a)
  }
  const byName = (a: PaperAccount, b: PaperAccount) =>
    a.name.localeCompare(b.name, 'ja')
  const groups: AccountGroup[] = []
  if (crypto.length) {
    groups.push({ key: 'crypto', label: '暗号資産', accounts: crypto.sort(byName) })
  }
  if (fx.length) {
    groups.push({ key: 'fx', label: 'FX', accounts: fx.sort(byName) })
  }
  if (other.length) {
    groups.push({ key: 'other', label: 'その他', accounts: other.sort(byName) })
  }
  return groups
}

export default function Trades() {
  const [period, setPeriod] = useState<string>('')
  const range = useMemo(() => periodToRange(period), [period])

  const { data: accounts, isLoading } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const groups = useMemo(
    () => buildGroups(accounts ?? []),
    [accounts],
  )

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'
  const labelClass = 'text-xs text-gray-400 mr-1'

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">トレード履歴</h2>

      <div className="bg-gray-900 rounded p-3 flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <span className={labelClass}>期間</span>
          <select
            value={period}
            onChange={(e) => setPeriod(e.target.value)}
            className={selectClass}
          >
            <option value="">全期間</option>
            <option value="today">今日</option>
            <option value="1w">1週間</option>
            <option value="1m">1ヶ月</option>
          </select>
        </div>
      </div>

      {isLoading ? (
        <div className="bg-gray-900 rounded p-8 text-center text-gray-500">
          読み込み中...
        </div>
      ) : groups.length === 0 ? (
        <div className="bg-gray-900 rounded p-8 text-center text-gray-500">
          口座が登録されていません
        </div>
      ) : (
        groups.map((group) => (
          <section key={group.key} className="space-y-3">
            <h3 className="text-sm font-semibold text-gray-400 uppercase tracking-wider">
              {group.label}
            </h3>
            <div className="space-y-4">
              {group.accounts.map((account) => (
                <TradeTable
                  key={account.id}
                  account={account}
                  from={range.from}
                  to={range.to}
                />
              ))}
            </div>
          </section>
        ))
      )}
    </div>
  )
}
```

- [ ] **Step 2: Run lint**

Run: `cd dashboard-ui && npm run lint`
Expected: PASS.

- [ ] **Step 3: Run build**

Run: `cd dashboard-ui && npm run build`
Expected: PASS (all TypeScript checks clean).

- [ ] **Step 4: Commit both files together**

The refactor only makes sense as a single commit since Task 1 leaves the tree in a broken state.

```bash
git add dashboard-ui/src/components/TradeTable.tsx dashboard-ui/src/pages/Trades.tsx
git commit -m "$(cat <<'EOF'
feat(ui): split trade history into per-account tables

Replace the single paginated Trades table with one table per paper
account, grouped by exchange (crypto first, then FX). Each table
shows the latest 10 trades for that account plus a header with the
account name, type badge, evaluated balance, cash balance, and
leverage.

Empty accounts still render so that silent paper-trading bugs are
visible at a glance. The account / exchange filters are removed
since the grouping makes them redundant; only the period filter
remains.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Manual smoke test

**Files:** None (verification only)

- [ ] **Step 1: Start the dashboard locally**

Run (from repo root): consult `README.md` / `docs/` for the standard startup; typically `docker compose up dashboard-ui` or `cd dashboard-ui && npm run dev`. Ask the user which command to use if unclear — do **not** guess.

- [ ] **Step 2: Navigate to the Trades tab and verify each checklist item**

Walk through the page and check each of the following visually. Note any failure before proceeding.

- [ ] 上部のフィルタは「期間」のみで、`投資種別` と `口座` のセレクタが消えている
- [ ] 期間を `1週間` に変えると、すべての口座テーブルが同時にローディング→再描画される
- [ ] グループ見出しは `暗号資産` → `FX` の順で表示される
- [ ] 各グループ内で口座名が昇順になっている
- [ ] 各テーブルの見出しに `口座名 [種別バッジ] 評価額 残高 レバレッジ` が表示される
- [ ] ペーパー口座には灰色バッジ、通常口座には青系バッジが付いている
- [ ] 行カラムから `口座` と `種別` が消えている
- [ ] 行クリックでタイムラインが従来通り展開する
- [ ] 該当期間にトレードが無い口座でも、見出しと `トレードデータがありません` が表示される
- [ ] ブラウザの開発者ツールで console / network エラーが出ていない

- [ ] **Step 3: Report findings back to the user**

Summarize what passed and anything that didn't match the spec. Do **not** proceed to commit/push if anything is broken — fix first, then re-verify.

---

## Post-Implementation

1. Run `code-review` skill on the diff before creating a PR (per project rules — `commit/push/PR前にcode-reviewスキル必須`).
2. Push branch and open PR. **Do NOT** perform any GitHub-side operations (push, gh pr ...) yourself — prepare the commands and let the user run them (per project feedback memory).
