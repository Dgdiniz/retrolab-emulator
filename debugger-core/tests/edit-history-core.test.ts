import { describe, expect, it } from 'vitest'
import { createEditHistory } from '../src/edit-history-core'

function makeClock(initial = 0): { now: () => number; advance: (ms: number) => void } {
  let t = initial
  return { now: () => t, advance: (ms) => { t += ms } }
}

describe('edit-history-core > createEditHistory', () => {
  it('starts empty', () => {
    const h = createEditHistory()
    expect(h.canUndo()).toBe(false)
    expect(h.canRedo()).toBe(false)
    expect(h.undo()).toBeNull()
    expect(h.redo()).toBeNull()
    expect(h.size()).toEqual({ undo: 0, redo: 0 })
  })

  it('pushes new entries and exposes them via peekUndo', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(10, 0x00, 0x11)
    expect(h.canUndo()).toBe(true)
    expect(h.peekUndo()).toMatchObject({ offset: 10, oldValue: 0x00, newValue: 0x11 })
    expect(h.size().undo).toBe(1)
  })

  it('undo returns entries in reverse order and feeds the redo stack', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(1, 0xaa, 0xbb)
    h.push(2, 0xcc, 0xdd)
    expect(h.undo()).toMatchObject({ offset: 2, newValue: 0xdd })
    expect(h.undo()).toMatchObject({ offset: 1, newValue: 0xbb })
    expect(h.undo()).toBeNull()
    expect(h.size()).toEqual({ undo: 0, redo: 2 })
  })

  it('redo replays in original order', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(1, 0xaa, 0xbb)
    h.push(2, 0xcc, 0xdd)
    h.undo()
    h.undo()
    expect(h.redo()).toMatchObject({ offset: 1 })
    expect(h.redo()).toMatchObject({ offset: 2 })
    expect(h.redo()).toBeNull()
  })

  it('clears the redo stack on a new push', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(1, 0x00, 0x10)
    h.push(2, 0x00, 0x20)
    h.undo()
    expect(h.canRedo()).toBe(true)
    h.push(3, 0x00, 0x30)
    expect(h.canRedo()).toBe(false)
  })

  it('coalesces successive edits at the same offset within the window', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 500 })
    const a = h.push(10, 0x00, 0x11)
    clock.advance(100)
    const b = h.push(10, 0x11, 0x22)
    expect(a).toBe(true)
    expect(b).toBe(false)
    expect(h.size().undo).toBe(1)
    expect(h.peekUndo()).toMatchObject({ offset: 10, oldValue: 0x00, newValue: 0x22 })
  })

  it('does not coalesce edits separated by more than the window', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 500 })
    h.push(10, 0x00, 0x11)
    clock.advance(501)
    h.push(10, 0x11, 0x22)
    expect(h.size().undo).toBe(2)
  })

  it('does not coalesce edits at different offsets', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 500 })
    h.push(10, 0x00, 0x11)
    h.push(11, 0x00, 0x22)
    expect(h.size().undo).toBe(2)
  })

  it('disables coalescing when coalesceWindowMs is zero', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(10, 0x00, 0x11)
    h.push(10, 0x11, 0x22)
    expect(h.size().undo).toBe(2)
  })

  it('drops the oldest entry when exceeding maxEntries', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0, maxEntries: 3 })
    h.push(1, 0, 1)
    h.push(2, 0, 2)
    h.push(3, 0, 3)
    h.push(4, 0, 4)
    expect(h.size().undo).toBe(3)
    // Oldest (offset 1) was dropped; remaining undo order is 4, 3, 2.
    expect(h.undo()).toMatchObject({ offset: 4 })
    expect(h.undo()).toMatchObject({ offset: 3 })
    expect(h.undo()).toMatchObject({ offset: 2 })
  })

  it('clears both stacks', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    h.push(1, 0, 1)
    h.push(2, 0, 2)
    h.undo() // populates redoStack
    expect(h.size()).toEqual({ undo: 1, redo: 1 })
    h.clear()
    expect(h.size()).toEqual({ undo: 0, redo: 0 })
  })

  it('coalesced push still clears redo', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 500 })
    h.push(1, 0x00, 0x10)
    h.push(2, 0x00, 0x20)
    h.undo()
    expect(h.canRedo()).toBe(true)
    // Coalesce with the surviving top entry (offset 1).
    h.push(1, 0x10, 0x11)
    expect(h.canRedo()).toBe(false)
  })

  it('exposes peekRedo without mutating the stack', () => {
    const clock = makeClock()
    const h = createEditHistory({ now: clock.now, coalesceWindowMs: 0 })
    expect(h.peekRedo()).toBeNull()
    h.push(1, 0x00, 0x10)
    h.undo()
    expect(h.peekRedo()).toMatchObject({ offset: 1, newValue: 0x10 })
    // Calling peek twice still leaves the entry on the redo stack.
    expect(h.peekRedo()).toMatchObject({ offset: 1 })
    expect(h.size().redo).toBe(1)
  })

  it('uses a wall clock by default (push timestamps are monotonic-ish)', () => {
    const h = createEditHistory({ coalesceWindowMs: 0 })
    h.push(1, 0, 1)
    h.push(2, 0, 2)
    const top = h.peekUndo()
    expect(top).not.toBeNull()
    expect(typeof top!.timestamp).toBe('number')
    expect(Number.isFinite(top!.timestamp)).toBe(true)
  })
})
