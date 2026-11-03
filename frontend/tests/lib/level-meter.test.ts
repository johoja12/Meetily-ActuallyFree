import { describe, expect, test } from 'bun:test';
import {
  BAR_GAP,
  FIXED_BAR_WIDTH,
  MIN_BAR_FRACTION,
  amplitudeOf,
  intrinsicMeterWidth,
  meterBars,
  pushLevel,
  visualLevel,
} from '../../src/lib/level-meter';

describe('level meter math', () => {
  test('decibel scale: silence on the floor, -16 dB full, gain applied first', () => {
    expect(visualLevel(0)).toBe(0);
    expect(visualLevel(10 ** (-50 / 20))).toBeCloseTo(0, 5);
    expect(visualLevel(10 ** (-16 / 20))).toBeCloseTo(1, 5);
    expect(visualLevel(10 ** (-33 / 20))).toBeCloseTo(0.5, 5);
    expect(visualLevel(10 ** (-39 / 20), 2)).toBeCloseTo(visualLevel(2 * 10 ** (-39 / 20)), 9);
  });

  test('amplitude uses the larger of peak and scaled RMS', () => {
    expect(amplitudeOf(0.1, 0.05)).toBeCloseTo(0.145, 9);
    expect(amplitudeOf(0.01, 0.2)).toBe(0.2);
    expect(amplitudeOf(-1, -1)).toBe(0);
  });

  test('filled bars share the width and grow from the bottom', () => {
    const bars = meterBars([0, 10 ** (-16 / 20), 0, 0], 100, 16, { fill: true, active: true });
    const width = (100 - 3 * BAR_GAP) / 4;
    expect(bars.map((b) => b.x)).toEqual([0, width + BAR_GAP, 2 * (width + BAR_GAP), 3 * (width + BAR_GAP)]);
    expect(bars[1].height).toBeCloseTo(16, 9);
    expect(bars[1].y).toBeCloseTo(0, 9);
    expect(bars[0].height).toBeCloseTo(16 * MIN_BAR_FRACTION, 9);
    expect(bars[0].y + bars[0].height).toBeCloseTo(16, 9);
    const last = bars[3];
    expect(last.x + last.width).toBeCloseTo(100, 9);
  });

  test('fixed bars keep their width; inactive meters are dimmed', () => {
    const bars = meterBars([1, 1, 1], 500, 16, { fill: false, active: false });
    expect(bars.every((b) => b.width === FIXED_BAR_WIDTH && b.alpha === 0.45)).toBe(true);
    expect(intrinsicMeterWidth(3)).toBe(3 * FIXED_BAR_WIDTH + 2 * BAR_GAP);
    expect(meterBars([1, 1, 1], 500, 16, { fill: false, active: true })[0].alpha).toBe(1);
  });

  test('empty or zero-size meters draw nothing', () => {
    expect(meterBars([], 100, 16, { fill: true, active: true })).toEqual([]);
    expect(meterBars([1], 0, 16, { fill: true, active: true })).toEqual([]);
  });

  test('history scrolls left with a fixed length', () => {
    expect(pushLevel([1, 2, 3], 4)).toEqual([2, 3, 4]);
  });
});
