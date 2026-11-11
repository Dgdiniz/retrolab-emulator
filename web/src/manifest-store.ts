/**
 * IndexedDB persistence for snes1 cartridge-manifest overrides, keyed by
 * ROM hash (the same `computeByteHash` key the SRAM / breakpoints stores
 * use). Mirrors the manifest file RetroLab keeps beside each game: a
 * per-ROM JSON blob applied via `setCartridgeManifest` before `loadRom`.
 *
 * This uses its own database (`retro-emulator-manifests`) rather than a
 * new object store inside the shared `retro-emulator-saves` DB so it
 * doesn't force a version bump that the SRAM / save-state managers would
 * also have to coordinate on.
 */

const DB_NAME = 'retro-emulator-manifests'
const DB_VERSION = 1
const STORE = 'manifests'

interface ManifestRecord {
  romHash: string
  json: string
  lastModified: string
}

let dbPromise: Promise<IDBDatabase> | null = null

function openDb(): Promise<IDBDatabase> {
  if (dbPromise) return dbPromise
  dbPromise = new Promise<IDBDatabase>((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION)
    req.onerror = () => reject(req.error)
    req.onsuccess = () => resolve(req.result)
    req.onupgradeneeded = () => {
      const db = req.result
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: 'romHash' })
      }
    }
  })
  return dbPromise
}

/** Load the stored override JSON for a ROM hash, or `null` when none. */
export async function loadManifestOverride(romHash: string): Promise<string | null> {
  const db = await openDb()
  return new Promise<string | null>((resolve, reject) => {
    const tx = db.transaction([STORE], 'readonly')
    const req = tx.objectStore(STORE).get(romHash)
    req.onsuccess = () => resolve((req.result as ManifestRecord | undefined)?.json ?? null)
    req.onerror = () => reject(req.error)
  })
}

/** Persist (overwrite) the override JSON for a ROM hash. */
export async function saveManifestOverride(romHash: string, json: string): Promise<void> {
  const db = await openDb()
  return new Promise<void>((resolve, reject) => {
    const tx = db.transaction([STORE], 'readwrite')
    const record: ManifestRecord = { romHash, json, lastModified: new Date().toISOString() }
    const req = tx.objectStore(STORE).put(record)
    req.onsuccess = () => resolve()
    req.onerror = () => reject(req.error)
  })
}

/** Remove the override for a ROM hash (idempotent). */
export async function deleteManifestOverride(romHash: string): Promise<void> {
  const db = await openDb()
  return new Promise<void>((resolve, reject) => {
    const tx = db.transaction([STORE], 'readwrite')
    const req = tx.objectStore(STORE).delete(romHash)
    req.onsuccess = () => resolve()
    req.onerror = () => reject(req.error)
  })
}
