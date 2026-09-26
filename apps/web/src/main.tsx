/** The one place that decides which backend this build talks to. */
import React from 'react'
import ReactDOM from 'react-dom/client'
import '@/styles.css'
import { App } from './App'
import { TransportProvider } from './TransportContext'
import { WebTransport } from './transport'

// Same origin by default: the server serves this bundle, so one container is
// the whole deployment. `VITE_LOUVER_API` is for running Vite's dev server
// against a server somewhere else.
const transport = new WebTransport(import.meta.env.VITE_LOUVER_API ?? '')

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <TransportProvider value={transport}>
      <App />
    </TransportProvider>
  </React.StrictMode>,
)
