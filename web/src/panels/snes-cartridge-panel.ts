/**
 * snes1 cartridge / manifest panel (standalone debugger).
 *
 * The Genesis cartridge tab is owned by `cartridge-debugger.js`
 * with a Sega-specific manifest editor (SSF2 / SVP / EEPROM). snes1 needs
 * a different shape (LoROM / HiROM / ExHiROM presets, mirrored SRAM size,
 * FastROM), so this is a dedicated panel that builds its UI from the
 * shared `@debugger-core/snes-manifest-core` option lists + validators,
 * the exact same module RetroLab's manifest editor uses, so the two stay in
 * lockstep.
 *
 * The override is persisted in IndexedDB keyed by ROM hash (see
 * `manifest-store.ts`) and applied via `setCartridgeManifest` before
 * `loadRom`: both on the next page-load (wired in `main.ts`) and on the
 * panel's "Apply & Reload" action.
 */

import {
  PRESET_OPTIONS,
  SRAM_SIZE_OPTIONS,
  applyFormToManifest,
  coprocessorLabel,
  formatSramSize,
  manifestDiffersFromDetected,
  manifestFormFromJson,
  parseDetectedManifest,
  parseManifestJson,
  stringifyManifest,
  validateManifestForm,
  type SnesManifest,
  type SnesManifestForm,
  type SnesPreset,
} from '@debugger-core/snes-manifest-core'
import type { DebugBridge, PanelHandle } from '../debug-bridge'
import {
  deleteManifestOverride,
  loadManifestOverride,
  saveManifestOverride,
} from '../manifest-store'

export interface SnesCartridgePanelHandle extends PanelHandle {
  /** Re-detect + reload the override after a fresh ROM load. */
  refresh(): void
}

const DEFAULT_FORM: SnesManifestForm = { preset: 'lorom', sramSize: 0, fastrom: false }

/** Resolve the override JSON to apply on the next `loadRom` for a hash. */
export async function readStoredManifest(hash: string | null): Promise<string | null> {
  if (!hash) return null
  try {
    return await loadManifestOverride(hash)
  } catch (e) {
    console.warn('[snes-cartridge] failed to read stored manifest:', e)
    return null
  }
}

export function initSnesCartridgePanel(bridge: DebugBridge): SnesCartridgePanelHandle {
  const root = document.getElementById('snes-manifest-root')
  if (!root) {
    throw new Error('snes cartridge panel requires #snes-manifest-root')
  }

  let detected: SnesManifest | null = null
  let override: SnesManifest | null = null
  let form: SnesManifestForm = { ...DEFAULT_FORM }
  let rawMode = false
  let rawText = ''
  let busy = false
  let notice = ''

  // -- DOM scaffold ---------------------------------------------------
  root.innerHTML = ''
  root.classList.add('snes-manifest')

  const detectedLine = div('snes-manifest-detected')
  const warnLine = div('snes-manifest-warn')

  const presetSel = document.createElement('select')
  for (const opt of PRESET_OPTIONS) presetSel.appendChild(option(opt.value, opt.label))
  const sramSel = document.createElement('select')
  const fastromChk = checkbox()
  const formWrap = div('snes-manifest-form')
  formWrap.appendChild(field('Mapper preset', presetSel))
  formWrap.appendChild(field('SRAM size', sramSel))
  formWrap.appendChild(field('FastROM (120 ns)', fastromChk))

  const rawArea = document.createElement('textarea')
  rawArea.className = 'snes-manifest-raw'
  rawArea.spellcheck = false
  rawArea.style.display = 'none'

  const errorList = document.createElement('ul')
  errorList.className = 'snes-manifest-errors'

  const rawToggle = checkbox()
  const saveBtn = button('Save')
  const applyBtn = button('Apply & Reload')
  const resetBtn = button('Reset to detected')
  const actions = div('snes-manifest-actions')
  actions.appendChild(label('Advanced (raw JSON)', rawToggle))
  const spacer = div('snes-manifest-spacer')
  actions.appendChild(spacer)
  actions.appendChild(resetBtn)
  actions.appendChild(saveBtn)
  actions.appendChild(applyBtn)

  const noticeLine = div('snes-manifest-notice')

  root.append(detectedLine, warnLine, formWrap, rawArea, errorList, actions, noticeLine)

  // -- Pure-ish resolution (mirrors RetroLab's manifest editor) --------
  function base(): SnesManifest | null {
    return override ?? detected
  }

  /** { json, errors } for the active editing surface, or json=null. */
  function resolve(): { json: string | null; manifest: SnesManifest | null; errors: string[] } {
    if (rawMode) {
      try {
        const parsed = parseManifestJson(rawText)
        return { json: stringifyManifest(parsed), manifest: parsed, errors: [] }
      } catch (e) {
        return { json: null, manifest: null, errors: [(e as Error).message] }
      }
    }
    const errs = validateManifestForm(form)
    if (errs.length) return { json: null, manifest: null, errors: errs }
    const b = base()
    if (!b) return { json: null, manifest: null, errors: ['No cartridge manifest available to edit'] }
    const m = applyFormToManifest(form, b)
    return { json: stringifyManifest(m), manifest: m, errors: [] }
  }

  function modified(): boolean {
    const { manifest } = resolve()
    return manifest ? manifestDiffersFromDetected(detected, manifest) : false
  }

  // -- Render ----------------------------------------------------------
  function render(): void {
    const rom = detected?.rom ?? null
    const cop = coprocessorLabel(detected)
    // Nothing to edit until the core hands us a detected manifest (a ROM
    // is loaded). Show only the hint and hide the editing surface so we
    // don't surface a redundant "no manifest" validation error.
    const empty = !detected && !override
    detectedLine.textContent = ''
    if (rom) {
      detectedLine.appendChild(span('snes-manifest-title', rom.title || 'Unknown title'))
      if (rom.region) detectedLine.appendChild(tag(rom.region))
      if (cop) detectedLine.appendChild(tag(cop))
      if (modified()) detectedLine.appendChild(tag('Modified', 'snes-manifest-tag--mod'))
      warnLine.style.display = 'none'
    } else {
      warnLine.textContent = 'No manifest detected — load a SNES (snes1) ROM to edit its mapping.'
      warnLine.style.display = ''
    }

    presetSel.value = form.preset
    rebuildSramOptions()
    sramSel.value = String(form.sramSize)
    fastromChk.checked = form.fastrom

    formWrap.style.display = empty || rawMode ? 'none' : ''
    rawArea.style.display = !empty && rawMode ? '' : 'none'
    rawToggle.checked = rawMode
    actions.style.display = empty ? 'none' : ''

    const { json, errors } = empty ? { json: null, errors: [] } : resolve()
    errorList.innerHTML = ''
    for (const e of errors) {
      const li = document.createElement('li')
      li.textContent = e
      errorList.appendChild(li)
    }

    const disabled = busy || json === null
    saveBtn.disabled = disabled
    applyBtn.disabled = disabled
    resetBtn.disabled = busy || (!modified() && override === null)
    presetSel.disabled = busy
    sramSel.disabled = busy
    fastromChk.disabled = busy
    rawArea.disabled = busy
    saveBtn.textContent = busy ? 'Saving…' : 'Save'

    noticeLine.textContent = notice
    noticeLine.style.display = notice ? '' : 'none'
  }

  function rebuildSramOptions(): void {
    sramSel.innerHTML = ''
    const known = SRAM_SIZE_OPTIONS.some((o) => o.value === form.sramSize)
    const opts = known
      ? SRAM_SIZE_OPTIONS
      : [...SRAM_SIZE_OPTIONS, { value: form.sramSize, label: formatSramSize(form.sramSize) }]
    for (const o of opts) sramSel.appendChild(option(String(o.value), o.label))
  }

  function seedForm(): void {
    const b = base()
    form = b ? manifestFormFromJson(b) : { ...DEFAULT_FORM }
    rawMode = false
    rawText = override ? stringifyManifest(override) : (detected ? stringifyManifest(detected) : '')
  }

  // -- Events ----------------------------------------------------------
  presetSel.addEventListener('change', () => { form = { ...form, preset: presetSel.value as SnesPreset }; render() })
  sramSel.addEventListener('change', () => { form = { ...form, sramSize: Number(sramSel.value) || 0 }; render() })
  fastromChk.addEventListener('change', () => { form = { ...form, fastrom: fastromChk.checked }; render() })
  rawArea.addEventListener('input', () => { rawText = rawArea.value; render() })
  rawToggle.addEventListener('change', () => {
    rawMode = rawToggle.checked
    // Entering raw mode seeds the textarea from the current form output so
    // the two surfaces start in sync.
    if (rawMode) {
      const { json } = resolve()
      if (json) rawText = json
      rawArea.value = rawText
    }
    render()
  })

  saveBtn.addEventListener('click', () => void onSave())
  applyBtn.addEventListener('click', () => void onApply())
  resetBtn.addEventListener('click', () => void onReset())

  async function persist(json: string): Promise<boolean> {
    const hash = bridge.getRomMeta().hash
    if (!hash) {
      notice = 'No ROM loaded — cannot persist a manifest override.'
      render()
      return false
    }
    busy = true; notice = ''; render()
    try {
      await saveManifestOverride(hash, json)
      override = parseManifestJson(json)
      notice = 'Saved.'
      return true
    } catch (e) {
      console.warn('[snes-cartridge] save failed:', e)
      notice = 'Failed to save the manifest.'
      return false
    } finally {
      busy = false; render()
    }
  }

  async function onSave(): Promise<void> {
    const { json } = resolve()
    if (json) await persist(json)
  }

  async function onApply(): Promise<void> {
    const { json } = resolve()
    if (!json) return
    if (!(await persist(json))) return
    const emu = bridge.getEmulator()
    const data = bridge.getRomMeta().data
    if (emu && data) {
      try {
        emu.setCartridgeManifest?.(json)
        emu.loadRom(data)
        bridge.refreshAll()
        notice = 'Applied — ROM reloaded with the override.'
      } catch (e) {
        console.warn('[snes-cartridge] reload failed:', e)
        notice = 'Saved, but reloading the ROM failed.'
      }
      render()
    }
  }

  async function onReset(): Promise<void> {
    const hash = bridge.getRomMeta().hash
    busy = true; notice = ''; render()
    try {
      if (hash) await deleteManifestOverride(hash)
      override = null
      seedForm()
      notice = 'Override removed — reverted to detected mapping.'
    } catch (e) {
      console.warn('[snes-cartridge] reset failed:', e)
      notice = 'Failed to remove the override.'
    } finally {
      busy = false; render()
    }
  }

  // -- Public refresh --------------------------------------------------
  async function refresh(): Promise<void> {
    const emu = bridge.getEmulator()
    detected = null
    if (emu && bridge.isRomLoaded()) {
      try {
        const json = emu.getDetectedManifest?.()
        detected = typeof json === 'string' ? parseDetectedManifest(json) : null
      } catch (e) {
        console.warn('[snes-cartridge] detect failed:', e)
      }
    }
    override = null
    try {
      const stored = await readStoredManifest(bridge.getRomMeta().hash)
      override = stored ? parseManifestJson(stored) : null
    } catch (e) {
      console.warn('[snes-cartridge] override parse failed:', e)
    }
    notice = ''
    seedForm()
    rawArea.value = rawText
    render()
  }

  render()

  return {
    update() { render() },
    refresh() { void refresh() },
    dispose() { root.innerHTML = '' },
  }
}

// -- tiny DOM helpers --------------------------------------------------
function div(cls: string): HTMLDivElement {
  const d = document.createElement('div')
  d.className = cls
  return d
}
function span(cls: string, text: string): HTMLSpanElement {
  const s = document.createElement('span')
  s.className = cls
  s.textContent = text
  return s
}
function tag(text: string, extra = ''): HTMLSpanElement {
  const s = span(`snes-manifest-tag${extra ? ' ' + extra : ''}`, text)
  return s
}
function option(value: string, label: string): HTMLOptionElement {
  const o = document.createElement('option')
  o.value = value
  o.textContent = label
  return o
}
function checkbox(): HTMLInputElement {
  const i = document.createElement('input')
  i.type = 'checkbox'
  return i
}
function button(text: string): HTMLButtonElement {
  const b = document.createElement('button')
  b.type = 'button'
  b.className = 'snes-manifest-btn'
  b.textContent = text
  return b
}
function field(labelText: string, control: HTMLElement): HTMLLabelElement {
  const l = document.createElement('label')
  l.className = 'snes-manifest-row'
  const s = document.createElement('span')
  s.className = 'snes-manifest-label'
  s.textContent = labelText
  l.append(s, control)
  return l
}
function label(text: string, control: HTMLElement): HTMLLabelElement {
  const l = document.createElement('label')
  l.className = 'snes-manifest-check'
  l.append(control, document.createTextNode(' ' + text))
  return l
}
