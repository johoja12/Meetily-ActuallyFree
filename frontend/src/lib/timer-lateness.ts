/** How often the hidden-window probe ticks. */
export const PROBE_INTERVAL_MS = 15_000;
/** Lateness worth logging: well past ordinary jitter, short of a sleep/wake gap. */
const REPORT_AFTER_MS = 20_000;
/** A gap this long is the machine sleeping, not the WebView being throttled. */
const SLEEP_GAP_MS = 30 * 60_000;

/** Milliseconds a tick arrived late, or null when it is not worth reporting. */
export function reportableLateness(previousTick: number, now: number, interval = PROBE_INTERVAL_MS): number | null {
  const late = now - previousTick - interval;
  return late >= REPORT_AFTER_MS && late < SLEEP_GAP_MS ? late : null;
}
