import { describe, expect, it } from 'vitest'
import { decimalsFor, fmtPrice } from './format'

// Regression: a fixed 2dp price format rendered every sub-dollar alt as "0.01"
// (ZORAUSDT at 0.0071880 showed "0.01" on the chart axis, tape, and ladder).
describe('decimalsFor', () => {
  it('keeps large prices tight', () => {
    expect(decimalsFor(62_685)).toBe(2)
    expect(decimalsFor(1_000)).toBe(2)
  })

  it('gives mid prices four decimals', () => {
    expect(decimalsFor(150.25)).toBe(4)
    expect(decimalsFor(1)).toBe(4)
  })

  it('scales sub-dollar prices to ~5 significant figures', () => {
    expect(decimalsFor(0.5784)).toBe(5) // ALCH-like
    expect(decimalsFor(0.05784)).toBe(6)
    expect(decimalsFor(0.007188)).toBe(7) // ZORA-like
    expect(decimalsFor(0.0000012)).toBe(8) // meme coin, capped
  })

  it('falls back to 2 for degenerate inputs', () => {
    expect(decimalsFor(null)).toBe(2)
    expect(decimalsFor(undefined)).toBe(2)
    expect(decimalsFor(0)).toBe(2)
    expect(decimalsFor(-5)).toBe(2)
    expect(decimalsFor(NaN)).toBe(2)
    expect(decimalsFor(Infinity)).toBe(2)
  })
})

describe('fmtPrice', () => {
  it('renders the magnitudes users actually see', () => {
    expect(fmtPrice(62_685)).toBe('62,685.00')
    expect(fmtPrice(0.05784)).toBe('0.057840')
    expect(fmtPrice(0.007188)).toBe('0.0071880')
  })

  it('renders nullish as an em dash', () => {
    expect(fmtPrice(null)).toBe('—')
    expect(fmtPrice(undefined)).toBe('—')
  })
})
