/**
 * Generic undo/redo stack for byte-level edits in a hex editor.
 *
 * The history is opaque (callers shouldn't poke its internals); push
 * an entry on every committed edit, then call `undo()` / `redo()` to
 * walk the stack. The entries carry both the previous and the new
 * value so the caller can re-apply or reverse a change without
 * touching this module.
 *
 * Consecutive edits to the same offset within `coalesceWindowMs`
 * collapse into a single entry (oldValue from the first push, newValue
 * from the latest). That matches what users intuitively expect when
 * they retype a byte they just committed, Ctrl+Z reverts to "before I
 * started touching this byte" rather than "before my last keystroke".
 *
 * Lives in the shared core so both UI shells (Vue and vanilla) get the
 * same coalesce semantics, and so the behaviour is unit-testable
 * without mounting a component.
 */

export interface EditEntry {
  offset: number
  oldValue: number
  newValue: number
  /** `performance.now()` at push time. Used by the coalesce window. */
  timestamp: number
}

export interface EditHistoryStats {
  undo: number
  redo: number
}

export interface EditHistoryOptions {
  /**
   * Pushes to the same `offset` within this window (ms) merge with the
   * previous entry instead of creating a new one. `0` disables
   * coalescing. Default 500ms.
   */
  coalesceWindowMs?: number
  /**
   * Hard cap on the undo stack. Once exceeded the oldest entries are
   * dropped. Default 1000.
   */
  maxEntries?: number
  /**
   * Time source. Tests inject a fake clock so coalescing windows are
   * deterministic. Defaults to `performance.now`.
   */
  now?: () => number
}

export interface EditHistory {
  /**
   * Record an edit. Clears the redo stack, once the user makes a new
   * change, the previously-undone edits can no longer be re-applied.
   *
   * Returns `true` when a new entry was pushed and `false` when the
   * edit was coalesced into the previous entry.
   */
  push(offset: number, oldValue: number, newValue: number): boolean
  /** Pop the most recent entry, push it onto the redo stack, and return it (or null when empty). */
  undo(): EditEntry | null
  /** Reverse of `undo`: pop from the redo stack back onto undo and return the entry. */
  redo(): EditEntry | null
  canUndo(): boolean
  canRedo(): boolean
  /** Clear both stacks. */
  clear(): void
  /** Counts for both stacks; useful for "X edits available" labels. */
  size(): EditHistoryStats
  /** Peek at the last entry without mutating either stack. */
  peekUndo(): EditEntry | null
  peekRedo(): EditEntry | null
}

export function createEditHistory(options: EditHistoryOptions = {}): EditHistory {
  const coalesceWindowMs = options.coalesceWindowMs ?? 500
  const maxEntries = Math.max(1, options.maxEntries ?? 1000)
  // `performance.now()` is available in browsers, all supported Node
  // versions, and Vitest's node environment, so we don't need a
  // `Date.now()` fallback path here.
  const now = options.now ?? (() => performance.now())

  const undoStack: EditEntry[] = []
  const redoStack: EditEntry[] = []

  function push(offset: number, oldValue: number, newValue: number): boolean {
    const t = now()
    const top = undoStack[undoStack.length - 1]
    const coalescable =
      coalesceWindowMs > 0 &&
      top !== undefined &&
      top.offset === offset &&
      t - top.timestamp <= coalesceWindowMs

    if (coalescable) {
      // Keep the original `oldValue` (so undo goes all the way back to
      // before the user started typing this byte) but adopt the latest
      // `newValue` and refresh the timestamp.
      top.newValue = newValue
      top.timestamp = t
      // A push still invalidates redo even when coalesced, the user
      // is making forward progress.
      redoStack.length = 0
      return false
    }

    undoStack.push({ offset, oldValue, newValue, timestamp: t })
    if (undoStack.length > maxEntries) {
      undoStack.shift()
    }
    redoStack.length = 0
    return true
  }

  function undo(): EditEntry | null {
    const entry = undoStack.pop()
    if (!entry) return null
    redoStack.push(entry)
    return entry
  }

  function redo(): EditEntry | null {
    const entry = redoStack.pop()
    if (!entry) return null
    undoStack.push(entry)
    return entry
  }

  return {
    push,
    undo,
    redo,
    canUndo: () => undoStack.length > 0,
    canRedo: () => redoStack.length > 0,
    clear() {
      undoStack.length = 0
      redoStack.length = 0
    },
    size: () => ({ undo: undoStack.length, redo: redoStack.length }),
    peekUndo: () => (undoStack.length > 0 ? undoStack[undoStack.length - 1] : null),
    peekRedo: () => (redoStack.length > 0 ? redoStack[redoStack.length - 1] : null),
  }
}
