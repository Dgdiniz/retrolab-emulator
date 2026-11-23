import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

import {
  manifestFromCartridgeInfo,
  memoryMapLegendText,
  regionColour,
} from '../src/cartridge-debugger.js'
import { describeShape, recordedIn } from './reader-shapes'

// What the Mega Drive binding's own test records of the cartridge's
// readers (`what_the_cartridge_readers_answer`): `getMemoryMap()` on a
// cartridge with save RAM and on an SVP cartridge, and `getCartridgeInfo()`
// on the one with save RAM.
const MEGA_DRIVE_RECORDS = 'cores/sg1/wasm/src/tests/binding_baseline.rs'
const saveRamMap = {
  regions: [
    { detail: '', end: 0x203fff, label: 'SRAM', start: 0x200000 },
    { detail: 'mask=0x07FFFF', end: 0x3fffff, label: 'ROM', start: 0 },
  ],
}
const svpMap = {
  regions: [
    { detail: 'base=0x100000', end: 0xa1500b, label: 'Chip 0', start: 0xa15000 },
    { detail: '', end: 0x31ffff, label: 'Chip 0', start: 0x300000 },
    { detail: 'base=0x200000', end: 0x39ffff, label: 'Chip 0', start: 0x390000 },
    { detail: 'base=0x300000', end: 0x3affff, label: 'Chip 0', start: 0x3a0000 },
    { detail: 'mask=0x07FFFF', end: 0x1fffff, label: 'ROM', start: 0 },
  ],
}
const saveRamInfo = {
  calculated_checksum: 47852,
  detection_source: 'Header',
  expansion_chip_info: undefined,
  header_checksum: 0,
  mapper_info: undefined,
  regions: ['Japan', 'USA', 'Europe'],
  rom_hash: '3e8ea16f75fdddd05ce46ad2acda5d99b2614035778606ed807dfaed1a64a042',
  rom_size: 524288,
  serial: undefined,
  sram_size: 16384,
  sram_type: 'Battery',
  system: 'Genesis',
  title: 'SAVING',
}

// A mapper as `getCartridgeInfo()` names it, retro-core's `MapperInfo`:
// the SNES binding's record of a LoROM cartridge shows its fields, and the
// Mega Drive's ids are the `Genesis` variants of retro-core's `MapperId`.
const loRom = { description: '', features: [], mapper_id: 'SnesLoROM', name: 'LoROM' }
const mapper = (mapperId: string, name: string) => ({ ...loRom, mapper_id: mapperId, name })
const svpChip = { chip_type: 'SVP', description: 'Sega Virtua Processor', features: [], name: 'SVP' }

describe("the cartridge debugger's memory map", () => {
  it('reads the regions as the binding records them', () => {
    const records = recordedIn(MEGA_DRIVE_RECORDS)
    expect(records).toContain(`getMemoryMap, save RAM => ${describeShape(saveRamMap)}`)
    expect(records).toContain(`getMemoryMap, SVP => ${describeShape(svpMap)}`)
  })

  it("writes each region's label, addresses and detail in the legend", () => {
    expect(saveRamMap.regions.map(memoryMapLegendText)).toEqual([
      'SRAM 0x200000-0x203FFF',
      'ROM 0x000000-0x3FFFFF (mask=0x07FFFF)',
    ])
    expect(memoryMapLegendText(svpMap.regions[0])).toBe('Chip 0 0xA15000-0xA1500B (base=0x100000)')
  })

  it('colours a region by its label: ROM as before, every other kind apart', () => {
    expect(regionColour('ROM')).toBe('#4CAF50')
    const others = ['SRAM', 'Chip 0', 'Chip 1', 'OpenBus'].map(regionColour)
    expect(new Set([regionColour('ROM'), ...others]).size).toBe(5)
  })
})

describe("the manifest editor's reset to what was detected", () => {
  it('reads the cartridge as the bindings record it', () => {
    const records = recordedIn(MEGA_DRIVE_RECORDS)
    expect(records).toContain(`getCartridgeInfo, save RAM => ${describeShape(saveRamInfo)}`)
    // The record of the SVP cartridge's line is cut short after its chip's type.
    expect(records).toContain('getCartridgeInfo, SVP => {calculated_checksum: 0, detection_source: "Header", expansion_chip_info: {chip_type: "SVP", descrip')
    expect(describeShape(svpChip)).toMatch(/^\{chip_type: "SVP", descrip/)
    expect(recordedIn('cores/snes1/wasm/src/tests/binding_baseline.rs')).toContain(
      `mapper_info: ${describeShape(loRom)}`,
    )
    const mapperIds = /pub enum MapperId \{([^}]*)\}/.exec(recordedIn('retro-core/src/cartridge.rs'))
    for (const id of ['GenesisSEGA', 'GenesisSSF2', 'GenesisCodemasters', 'GenesisEEPROM', 'GenesisJCart']) {
      expect(mapperIds?.[1]).toMatch(new RegExp(`\\b${id},`))
    }
  })

  it("fills the form with the cartridge's title, region boxes and save RAM", () => {
    expect(manifestFromCartridgeInfo(saveRamInfo)).toEqual({
      title: 'SAVING',
      regions: ['J', 'U', 'E'],
      mapper: 'None',
      chip: 'None',
      eeprom: 'None',
      sram: { start: '0x200000', end: '0x203FFF', odd_only: false },
    })
  })

  it('ticks no box for a region the form has none for', () => {
    expect(manifestFromCartridgeInfo({ ...saveRamInfo, regions: ['World'] }).regions).toEqual([])
    expect(manifestFromCartridgeInfo({ ...saveRamInfo, regions: ['Brazil', 'USA'] }).regions).toEqual(['U'])
  })

  it('names the mapper and the chip the cartridge has', () => {
    const reset = (info: object) => manifestFromCartridgeInfo({ ...saveRamInfo, ...info })
    expect(reset({ mapper_info: mapper('GenesisSEGA', 'SEGA') }).mapper).toBe('Sega')
    expect(reset({ mapper_info: mapper('GenesisSSF2', 'SSF2') }).mapper).toBe('SSF2')
    expect(reset({ mapper_info: mapper('GenesisCodemasters', 'Codemasters') }).mapper).toBe('Codemasters')
    expect(reset({ mapper_info: mapper('GenesisNone', 'None') }).mapper).toBe('None')
    const jCart = reset({ mapper_info: mapper('GenesisJCart', 'J-Cart') })
    expect([jCart.mapper, jCart.chip]).toEqual(['None', 'JCart'])
    expect(reset({ expansion_chip_info: svpChip, sram_size: 0 })).toMatchObject({ chip: 'SVP' })
  })

  it('gives an EEPROM cartridge its EEPROM, and no SRAM: its save is the EEPROM', () => {
    const eeprom = manifestFromCartridgeInfo({
      ...saveRamInfo,
      mapper_info: mapper('GenesisEEPROM', 'EEPROM'),
      sram_size: 128,
    })
    expect(eeprom).toMatchObject({ mapper: 'None', eeprom: 'I2C' })
    expect(eeprom.sram).toBeUndefined()
  })

  it('gives only values the form offers', () => {
    const page = readFileSync(fileURLToPath(new URL('../index.html', import.meta.url)), 'utf8')
    for (const value of ['None', 'Sega', 'SSF2', 'Codemasters', 'SVP', 'JCart', 'I2C']) {
      expect(page).toContain(`<option value="${value}">`)
    }
    for (const box of ['J', 'U', 'E']) {
      expect(page).toContain(`id="region-${box.toLowerCase()}" value="${box}"`)
    }
  })
})
