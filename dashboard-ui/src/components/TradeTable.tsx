import { useEffect, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  useReactTable,
  getCoreRowModel,
  createColumnHelper,
  flexRender,
} from '@tanstack/react-table'
import { api } from '../api/client'
import type { TradeRow } from '../api/types'

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

function buildColumns(accountMap: Map<string, string>) {
  return [
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

  useEffect(() => {
    setPage(1)
  }, [filters])

  const { data: accounts } = useQuery({
    queryKey: ['accounts'],
    queryFn: () => api.accounts.list(),
  })

  const accountMap = useMemo(() => {
    const m = new Map<string, string>()
    accounts?.forEach((a) => m.set(a.id, a.name))
    return m
  }, [accounts])

  const columns = useMemo(() => buildColumns(accountMap), [accountMap])

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
              table.getRowModel().rows.map((row) => (
                <tr key={row.id} className="border-b border-gray-800/50 hover:bg-gray-800/30">
                  {row.getVisibleCells().map((cell) => (
                    <td key={cell.id} className="px-3 py-2 whitespace-nowrap">
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </td>
                  ))}
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>

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
