import '@testing-library/jest-dom/vitest'
import { afterEach, vi } from 'vitest'
import { cleanup } from '@testing-library/react'
import { setMockBackend } from '@/services/ipc'

afterEach(() => {
  cleanup()
  setMockBackend(null)
  localStorage.clear()
  vi.restoreAllMocks()
})

// jsdom lacks these; the UI touches both.
if (!window.matchMedia) {
  window.matchMedia = ((q: string) => ({
    matches: false, media: q, onchange: null,
    addListener: () => {}, removeListener: () => {},
    addEventListener: () => {}, removeEventListener: () => {}, dispatchEvent: () => false,
  })) as typeof window.matchMedia
}
