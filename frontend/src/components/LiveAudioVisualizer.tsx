'use client';

/**
 * Live per-source audio level meter (mic OR system): a small scrolling bar
 * history drawn on one canvas.
 *
 * Levels are pushed from Rust via the `recording-audio-levels` event (pre-mix,
 * per source) — the webview cannot read system audio, so these meters are
 * intentionally Rust-driven rather than computed in JS.
 *
 * `fill` makes the bar row flex to fill its container (used by the stacked
 * Mic/System meters in RecordingControls and the minibar); without it the
 * component renders at its intrinsic width.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { LOW_SYSTEM_AUDIO_MESSAGE, SystemAudioLevelMonitor } from '@/lib/system-audio-level';
import { amplitudeOf, intrinsicMeterWidth, meterBars, pushLevel } from '@/lib/level-meter';

/**
 * Per-source live audio level sample emitted by the Rust audio pipeline
 * (see `AudioLevels` in `src-tauri/src/audio/pipeline.rs`). One event is sent
 * per incoming chunk, throttled to ~25/sec per source.
 */
interface RecordingAudioLevels {
  source: 'mic' | 'system' | string;
  rms: number;
  peak: number;
  limiter_hit?: boolean;
}

interface LiveAudioVisualizerProps {
  /** Whether recording is active and un-paused. When false the meter idles flat. */
  active: boolean;
  /** Which audio source to visualize: your microphone or the system/other participants. */
  source?: 'mic' | 'system';
  /** Number of history bars to render (acts like a small scrolling VU meter). */
  bars?: number;
  /** Stretch the bars to fill the container width instead of a fixed 3px each. */
  fill?: boolean;
  className?: string;
  /**
   * Preview samples (device picker) instead of the recording-audio-levels event.
   * `tick` advances once per sample so the history scrolls the same way as live.
   */
  feedRms?: number;
  feedPeak?: number;
  feedTick?: number;
  /** Multiplies the drawn level. Preview meters use this so the slider changes the graph immediately. */
  displayGain?: number;
}

/**
 * A compact, event-driven audio level meter. It subscribes to the Rust
 * `recording-audio-levels` event and renders a small row of bars that scrolls
 * as new samples arrive. Unlike a `getUserMedia`-based visualizer, this does
 * NOT open a second microphone stream (which would compete with the recording
 * capture) and it can faithfully show *system* audio, which the webview cannot
 * capture on its own.
 *
 * Rendering: levels arrive about 16 times a second per source for the whole
 * recording. Drawing them on a canvas from the event handler, outside React
 * state, avoids re-rendering and restyling every bar on each update. The
 * earlier DOM bars each carried a CSS transition that never finished at this
 * rate, so WebKit kept dozens of composited layers and animations alive; that
 * was most of the recording window's CPU (issue #21).
 */
export function LiveAudioVisualizer({
  active,
  source = 'mic',
  bars = 6,
  fill = false,
  className = '',
  feedRms,
  feedPeak,
  feedTick,
  displayGain = 1,
}: LiveAudioVisualizerProps) {
  const previewMode = feedTick !== undefined;
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const frameRef = useRef<number | null>(null);
  const drawStateRef = useRef({ active, fill, displayGain });
  drawStateRef.current = { active, fill, displayGain };
  const [limiterWarning, setLimiterWarning] = useState(false);
  const [lowWarning, setLowWarning] = useState(false);
  const lowMonitorRef = useRef(new SystemAudioLevelMonitor());
  const lowClearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const levelsRef = useRef<number[]>(new Array(bars).fill(0));

  const draw = useCallback(() => {
    frameRef.current = null;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const width = canvas.clientWidth;
    const height = canvas.clientHeight;
    const scale = window.devicePixelRatio || 1;
    const pixelWidth = Math.round(width * scale);
    const pixelHeight = Math.round(height * scale);
    if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
    if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
    const context = canvas.getContext('2d');
    if (!context) return;
    const { active: isActive, fill: isFill, displayGain: gain } = drawStateRef.current;
    context.setTransform(scale, 0, 0, scale, 0, 0);
    context.clearRect(0, 0, width, height);
    // The canvas's CSS color is the resolved bar color (theme tokens included).
    context.fillStyle = getComputedStyle(canvas).color;
    for (const bar of meterBars(levelsRef.current, width, height, { fill: isFill, active: isActive, gain })) {
      context.globalAlpha = bar.alpha;
      context.beginPath();
      if (typeof context.roundRect === 'function') context.roundRect(bar.x, bar.y, bar.width, bar.height, Math.min(2, bar.width / 2));
      else context.rect(bar.x, bar.y, bar.width, bar.height);
      context.fill();
    }
    context.globalAlpha = 1;
  }, []);

  // At most one draw per display frame, however many events arrive. Without
  // a display (tests, server render) there is nothing to draw on.
  const scheduleDraw = useCallback(() => {
    if (typeof requestAnimationFrame !== 'function') return;
    if (frameRef.current === null) frameRef.current = requestAnimationFrame(draw);
  }, [draw]);

  const setLevels = useCallback((next: number[]) => {
    levelsRef.current = next;
    scheduleDraw();
  }, [scheduleDraw]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(scheduleDraw);
    observer?.observe(canvas);
    return () => {
      observer?.disconnect();
      if (frameRef.current !== null && typeof cancelAnimationFrame === 'function') cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    };
  }, [scheduleDraw]);
  const limiterStartedAtRef = useRef<number | null>(null);
  const limiterLastHitAtRef = useRef<number | null>(null);
  const limiterClearTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Reset the history buffer when the bar count changes.
  useEffect(() => {
    setLevels(new Array(bars).fill(0));
  }, [bars, setLevels]);

  useEffect(() => {
    if (!previewMode) return;
    setLevels(pushLevel(levelsRef.current, amplitudeOf(feedRms ?? 0, feedPeak ?? 0)));
  }, [previewMode, feedTick, feedRms, feedPeak, setLevels]);

  useEffect(() => {
    if (previewMode) return;
    if (!active) {
      lowMonitorRef.current.reset();
      setLowWarning(false);
      setLevels(new Array(bars).fill(0));
      limiterStartedAtRef.current = null;
      limiterLastHitAtRef.current = null;
      setLimiterWarning(false);
      if (limiterClearTimerRef.current) {
        clearTimeout(limiterClearTimerRef.current);
        limiterClearTimerRef.current = null;
      }
      return;
    }

    let mounted = true;
    let unlisten: (() => void) | undefined;

    (async () => {
      try {
        const stopListening = await listen<RecordingAudioLevels>('recording-audio-levels', (event) => {
          if (!mounted) return;
          const payload = event.payload;
          if (payload.source !== source) return;

          setLevels(pushLevel(levelsRef.current, amplitudeOf(payload.rms, payload.peak)));

          if (source === 'system') {
            // Native recording levels already include system gain. displayGain
            // belongs to the device preview and must not amplify this check twice.
            setLowWarning(lowMonitorRef.current.update(payload.rms, payload.peak, performance.now()));
            if (lowClearTimerRef.current) clearTimeout(lowClearTimerRef.current);
            // Some backends stop sending events during silence or device loss.
            lowClearTimerRef.current = setTimeout(() => {
              lowMonitorRef.current.reset();
              setLowWarning(false);
            }, 1000);
          }

          if (source === 'system' && payload.limiter_hit) {
            const now = performance.now();
            const lastHit = limiterLastHitAtRef.current;
            if (lastHit === null || now - lastHit > 250) {
              limiterStartedAtRef.current = now;
            }
            limiterLastHitAtRef.current = now;

            const startedAt = limiterStartedAtRef.current;
            if (startedAt !== null && now - startedAt >= 750) {
              setLimiterWarning(true);
            }

            if (limiterClearTimerRef.current) {
              clearTimeout(limiterClearTimerRef.current);
            }
            limiterClearTimerRef.current = setTimeout(() => {
              limiterStartedAtRef.current = null;
              limiterLastHitAtRef.current = null;
              setLimiterWarning(false);
            }, 1500);
          }
        });
        if (mounted) {
          unlisten = stopListening;
        } else {
          stopListening();
        }
      } catch {
        // Not in a Tauri context (e.g. plain browser dev) — silently idle.
      }
    })();

    return () => {
      mounted = false;
      if (unlisten) unlisten();
      if (lowClearTimerRef.current) {
        clearTimeout(lowClearTimerRef.current);
        lowClearTimerRef.current = null;
      }
      lowMonitorRef.current.reset();
      setLowWarning(false);
      if (limiterClearTimerRef.current) {
        clearTimeout(limiterClearTimerRef.current);
        limiterClearTimerRef.current = null;
      }
    };
  }, [active, source, bars, previewMode, setLevels]);

  // Color, gain or fill changed: redraw the current history.
  useEffect(() => { scheduleDraw(); });

  // Your voice in the accent color, everyone else in violet, amber when clipping.
  const barColor = !active
    ? 'var(--af-text-4)'
    : limiterWarning || lowWarning
      ? 'var(--af-warning)'
      : source === 'mic'
        ? 'var(--af-accent)'
        : 'var(--af-c-violet)';
  const warningText = 'System audio is hitting the limiter. Lower system gain or playback volume.';
  const warning = !previewMode && active && (limiterWarning || lowWarning);
  const advice = limiterWarning ? warningText : LOW_SYSTEM_AUDIO_MESSAGE;

  return (
    <div
      className={`flex items-end gap-[2px] h-4 ${fill ? 'w-full' : ''} ${className}`}
      role="group"
      aria-label={`${source === 'mic' ? 'Microphone' : 'System'} audio level${warning ? limiterWarning ? '. Too loud.' : '. Very quiet.' : ''}`}
      title={warning ? advice : undefined}
    >
      <canvas
        ref={canvasRef}
        aria-hidden="true"
        className={`block h-full ${fill ? 'min-w-0 flex-1' : 'shrink-0'}`}
        style={{ color: barColor, width: fill ? undefined : intrinsicMeterWidth(bars) }}
      />
      {warning && <span role="status" tabIndex={0} title={advice} aria-label={advice} className="shrink-0 self-center whitespace-nowrap text-[10px] font-medium text-[var(--af-warning)]">
        {limiterWarning ? 'Too loud' : 'Low audio'}
      </span>}
    </div>
  );
}
