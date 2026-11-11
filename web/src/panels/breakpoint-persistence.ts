/**
 * Shared IndexedDB persistence for the whole breakpoint list, keyed by
 * ROM hash. Both the breakpoints panel (execute/read/write) and the
 * watchpoints panel (value-matched memory watchpoints + system-event
 * breakpoints) operate on the *same* WASM breakpoint list, so they must
 * share one store and one save/load path, two independent
 * "remove-all + re-add" loaders would clobber each other.
 *
 * The store is the single source of truth on restore: `loadBreakpoints`
 * clears the live list and rebuilds it from the saved entries. Mutations
 * in either panel call `saveBreakpoints` to snapshot the full list.
 *
 * Entries carry lossless E/R/W flags (`access`), the advanced match
 * conditions (`match_spec`, omitted when bare), and the event ref
 * (`event`) so value watchpoints and event breakpoints round-trip
 * alongside plain breakpoints. Older entries with only a legacy `bp_type`
 * still load (folded into flags by `parseAccessFlags`).
 */

import {
  accessToShorthand,
  isEmptyAccess,
  parseAccessFlags,
  type AccessFlags,
  type RawBpType,
} from '@debugger-core/breakpoints-core'
import {
  isDefaultMatchSpec,
  matchSpecToWire,
  parseEventTag,
  parseMatchSpec,
} from '@debugger-core/watchpoints-core'
import type { DebugBridge } from '../debug-bridge'

const DB_NAME = 'retrolab-breakpoints'
const DB_VERSION = 1
const STORE_NAME = 'breakpoints'

export interface StoredBreakpoint {
  address: number
  /** Lossless E/R/W flags, source of truth on restore. */
  access?: AccessFlags
  /** Best-fit shorthand kept for back-compat / legacy entries. */
  bp_type?: unknown
  cpu_id?: number
  enabled: boolean
  /** serde (snake_case) match spec, omitted when the spec is bare. */
  match_spec?: unknown
  /** Event ref for system-event breakpoints. */
  event?: { code: number; param: number }
}

let dbHandle: IDBDatabase | null = null

function openDb(): Promise<IDBDatabase | null> {
  if (dbHandle) return Promise.resolve(dbHandle)
  return new Promise<IDBDatabase | null>((resolve) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION)
    req.onerror = () => {
      console.warn('Failed to open breakpoint DB:', req.error)
      resolve(null)
    }
    req.onsuccess = () => {
      dbHandle = req.result
      resolve(dbHandle)
    }
    req.onupgradeneeded = (event) => {
      const db = (event.target as IDBOpenDBRequest).result
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: 'romHash' })
      }
    }
  })
}

/** Snapshot the full WASM breakpoint list into IndexedDB for this ROM. */
export async function saveBreakpoints(bridge: DebugBridge): Promise<void> {
  const meta = bridge.getRomMeta()
  const emulator = bridge.getEmulator()
  if (!meta.hash || !emulator) return
  try {
    const db = await openDb()
    if (!db) return

    const breakpoints: any[] = emulator.listBreakpoints() ?? []
    const bpData: StoredBreakpoint[] = breakpoints.map((bp: any) => {
      const rawType = bp.breakpoint.bp_type as RawBpType
      const event = parseEventTag(rawType)
      const access = parseAccessFlags(rawType)
      const spec = parseMatchSpec(bp.match_spec)
      return {
        address: bp.breakpoint.address,
        access,
        bp_type: accessToShorthand(access),
        cpu_id: bp.breakpoint.cpu_id,
        enabled: bp.enabled,
        match_spec: isDefaultMatchSpec(spec) ? undefined : matchSpecToWire(spec),
        event: event ? { code: event.code, param: event.param } : undefined,
      }
    })

    const tx = db.transaction(STORE_NAME, 'readwrite')
    tx.objectStore(STORE_NAME).put({ romHash: meta.hash, breakpoints: bpData })

    await new Promise<void>((resolve, reject) => {
      tx.oncomplete = () => resolve()
      tx.onerror = () => reject(tx.error)
    })
  } catch (err) {
    console.warn('Failed to save breakpoints:', err)
  }
}

/**
 * Restore the saved breakpoint list for the current ROM, replacing the
 * live list. Returns the number of restored entries (0 when nothing was
 * stored). Event breakpoints are skipped on cores without
 * `addEventBreakpoint`.
 */
export async function loadBreakpoints(bridge: DebugBridge): Promise<number> {
  const meta = bridge.getRomMeta()
  const emulator = bridge.getEmulator()
  if (!meta.hash || !emulator) return 0
  try {
    const db = await openDb()
    if (!db) return 0

    const tx = db.transaction(STORE_NAME, 'readonly')
    const req = tx.objectStore(STORE_NAME).get(meta.hash)
    const result = await new Promise<any>((resolve, reject) => {
      req.onsuccess = () => resolve(req.result)
      req.onerror = () => reject(req.error)
    })

    // Per-game isolation: the emulator instance can outlive a ROM swap, so
    // the previous ROM's breakpoints are still live here. Always wipe them
    // BEFORE deciding whether there's a saved set to apply, otherwise a
    // ROM with no stored breakpoints inherits the prior ROM's list (and a
    // ROM that does have a set would stack on top of the old one).
    const existing: any[] = emulator.listBreakpoints() ?? []
    for (const bp of existing) emulator.removeBreakpoint(bp.id)

    if (!result?.breakpoints?.length) return 0

    for (const bp of result.breakpoints as StoredBreakpoint[]) {
      if (bp.event) {
        if (typeof emulator.addEventBreakpoint !== 'function') continue
        const id = emulator.addEventBreakpoint(bp.event.code, bp.event.param)
        if (bp.match_spec) emulator.setBreakpointMatchSpec(id, bp.match_spec)
        if (!bp.enabled) emulator.enableBreakpoint(id, false)
        continue
      }
      // `access` is the source of truth; older entries only have the
      // legacy `bp_type` (bare string or tagged object), which
      // `parseAccessFlags` folds into flags too.
      const flags = bp.access ?? parseAccessFlags(bp.bp_type as RawBpType)
      if (isEmptyAccess(flags)) continue
      const id = emulator.addBreakpoint(bp.address, accessToShorthand(flags), bp.cpu_id ?? 0)
      emulator.setBreakpointAccess(id, flags.exec, flags.read, flags.write)
      if (bp.match_spec) emulator.setBreakpointMatchSpec(id, bp.match_spec)
      if (!bp.enabled) emulator.enableBreakpoint(id, false)
    }

    return result.breakpoints.length
  } catch (err) {
    console.warn('Failed to load breakpoints:', err)
    return 0
  }
}
