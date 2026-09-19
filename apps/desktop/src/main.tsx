import React from 'react'
import ReactDOM from 'react-dom/client'
import { App } from './App'
import './styles.css'
import { isTauri, setMockBackend } from '@/services/ipc'

// Running in a plain browser (`npm run dev` without Tauri) is useful for UI
// work, so a development backend stands in for Rust.
async function boot() {
  if (!isTauri()) {
    const { createMockBackend } = await import('@/test/mockBackend')
    setMockBackend(createMockBackend())
  }
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  )
}

void boot()
