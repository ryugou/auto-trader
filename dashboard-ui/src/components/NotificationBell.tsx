import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import NotificationDropdown from './NotificationDropdown'

export default function NotificationBell() {
  const [open, setOpen] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)
  const queryClient = useQueryClient()

  // Poll the lightweight unread-count endpoint. Inherits the shared
  // query client's 15s refetchInterval automatically.
  const { data: unreadData } = useQuery({
    queryKey: ['notifications-unread-count'],
    queryFn: () => api.notifications.unreadCount(),
  })

  const markAllRead = useMutation({
    mutationFn: () => api.notifications.markAllRead(),
    onSuccess: () => {
      // Invalidate both the badge and the dropdown list so they
      // reflect the now-read state on the next render.
      queryClient.invalidateQueries({ queryKey: ['notifications-unread-count'] })
      queryClient.invalidateQueries({ queryKey: ['notifications'] })
    },
    onError: (err) => {
      // We have no toast infrastructure. The unread-count query will
      // self-correct on the next 15s poll, so the user-visible
      // recovery is automatic — but a silent failure here would
      // strand the next debugging session without a breadcrumb.
      console.error('failed to mark notifications as read', err)
    },
  })

  // Close the dropdown on any mousedown outside the container. Using
  // mousedown (not click) matches the convention users expect from
  // other dropdowns so touching outside-and-releasing-inside doesn't
  // keep it open.
  useEffect(() => {
    if (!open) return
    const handler = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [open])

  const toggle = () => {
    const next = !open
    setOpen(next)
    // Fire mark-all-read exactly once, at the moment of opening. The
    // mutation is idempotent on the DB side (WHERE read_at IS NULL),
    // so duplicate fires would be harmless, but we still guard with
    // `next` to skip the call on close.
    if (next) {
      markAllRead.mutate()
    }
  }

  const count = unreadData?.count ?? 0
  const badgeText = count > 99 ? '99+' : String(count)

  return (
    <div ref={containerRef} className="relative ml-auto">
      <button
        type="button"
        onClick={toggle}
        aria-label="通知"
        aria-expanded={open}
        // Intentionally no `aria-haspopup`. The dropdown is a simple
        // popover (no focus trap, no menu navigation, items are
        // non-interactive) so claiming "dialog" or "menu" would lie
        // to assistive tech. `aria-expanded` alone is sufficient.
        className="relative p-1.5 text-gray-400 hover:text-gray-100 rounded transition"
      >
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
          <path d="M13.73 21a2 2 0 0 1-3.46 0" />
        </svg>
        {count > 0 && (
          <span className="absolute -top-0.5 -right-0.5 min-w-[16px] h-[16px] px-1 bg-red-500 text-white text-[10px] font-semibold rounded-full flex items-center justify-center">
            {badgeText}
          </span>
        )}
      </button>
      <NotificationDropdown open={open} />
    </div>
  )
}
