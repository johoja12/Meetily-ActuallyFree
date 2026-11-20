/**
 * Pure math for the live level meters (components/LiveAudioVisualizer.tsx).
 * The meter draws on a canvas, so geometry lives here where it can be tested.
 */

export const SILENCE_DB = -50;
export const FULL_DB = -16;
/** Bar width when the meter is not stretched to its container, in CSS pixels. */
export const FIXED_BAR_WIDTH = 3;
export const BAR_GAP = 2;
/** The shortest drawn bar, as a fraction of the meter height, so silence still shows a row. */
export const MIN_BAR_FRACTION = 0.04;

/** Linear amplitude from an RMS/peak pair. Speech on a raw device is often well below 0.05. */
export function amplitudeOf(rms: number, peak: number): number {
  return Math.max(0, peak, rms * 1.45);
}

/**
 * Map amplitude to a 0..1 meter using a speech-range decibel scale.
 * -56 dB sits on the floor. About -30 dB (normal speech) is mid-meter.
 * -9 dB is full. `gain` is the volume slider, applied before the decibel
 * conversion so the bars show output level, not the raw input.
 */
export function visualLevel(amplitude: number, gain = 1): number {
  const output = amplitude * Math.max(0.05, gain);
  const db = 20 * Math.log10(Math.max(output, 1e-6));
  const span = (db - SILENCE_DB) / (FULL_DB - SILENCE_DB);
  return Math.min(1, Math.max(0, span));
}

/** Width of a meter that is not stretched: fixed bars and gaps. */
export function intrinsicMeterWidth(bars: number): number {
  return bars * FIXED_BAR_WIDTH + Math.max(0, bars - 1) * BAR_GAP;
}

export interface BarRect {
  x: number;
  y: number;
  width: number;
  height: number;
  /** Opacity, matching the earlier DOM meter. */
  alpha: number;
}

/**
 * Bars in CSS pixels for a meter `width` x `height`, oldest level first. With
 * `fill` the bars share the width (at least 1 px each); otherwise they are
 * FIXED_BAR_WIDTH wide from the left. Bars grow from the bottom.
 */
export function meterBars(
  levels: readonly number[],
  width: number,
  height: number,
  options: { fill: boolean; active: boolean; gain?: number },
): BarRect[] {
  const count = levels.length;
  if (count === 0 || width <= 0 || height <= 0) return [];
  const barWidth = options.fill
    ? Math.max(1, (width - BAR_GAP * (count - 1)) / count)
    : FIXED_BAR_WIDTH;
  return levels.map((level, index) => {
    const shown = visualLevel(level, options.gain ?? 1);
    const barHeight = height * Math.max(MIN_BAR_FRACTION, shown);
    return {
      x: index * (barWidth + BAR_GAP),
      y: height - barHeight,
      width: barWidth,
      height: barHeight,
      alpha: options.active ? 0.45 + shown * 0.55 : 0.45,
    };
  });
}

/** Drop the oldest level and append `level`, keeping the history length. */
export function pushLevel(history: readonly number[], level: number): number[] {
  const next = history.slice(1);
  next.push(level);
  return next;
}
