/**
 * Where the transport is chosen, once.
 *
 * Every screen below this provider takes what it needs from `useTransport()`,
 * which is what §11 asks for: no component knows whether it is talking to a
 * server or to a desktop binary.
 */
import { createContext, useContext } from 'react'
import type { ReactNode } from 'react'
import type { Transport } from './transport'

const Ctx = createContext<Transport | null>(null)

export function TransportProvider({ value, children }: { value: Transport; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>
}

export function useTransport(): Transport {
  const t = useContext(Ctx)
  if (!t) throw new Error('TransportProvider가 없습니다')
  return t
}
