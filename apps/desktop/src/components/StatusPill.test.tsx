import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import { StatusPill } from './StatusPill'

describe('StatusPill', () => {
  it('shows OFFLINE when idle or stopped', () => {
    const { rerender } = render(<StatusPill state="IDLE" />)
    expect(screen.getByText('OFFLINE')).toBeInTheDocument()
    rerender(<StatusPill state="STOPPED" />)
    expect(screen.getByText('OFFLINE')).toBeInTheDocument()
  })

  it('shows LIVE only for a real broadcast', () => {
    render(<StatusPill state="LIVE" />)
    expect(screen.getByText('LIVE')).toBeInTheDocument()
  })

  it('distinguishes a dry run from a real broadcast', () => {
    // §30: a local test must never look like it is on air.
    render(<StatusPill state="LIVE" dryRun />)
    expect(screen.getByText('TEST')).toBeInTheDocument()
    expect(screen.queryByText('LIVE')).not.toBeInTheDocument()
  })

  it('surfaces reconnection rather than pretending to still be live', () => {
    render(<StatusPill state="RECONNECTING" />)
    expect(screen.getByText('재연결 중')).toBeInTheDocument()
    expect(screen.getByTestId('status-pill')).toHaveAttribute('data-state', 'RECONNECTING')
  })
})
