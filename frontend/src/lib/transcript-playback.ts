/** Times are recording-relative seconds, as the audio element reports them. */
export interface TimedSpan {
  timestamp: number;
  endTime?: number | null;
}

/**
 * The span playing at `time` (sorted spans): the last one that has started.
 * Between spans the last one stays lit until the next starts. -1 before the
 * first span or without a time.
 */
export function activeIndex(spans: readonly TimedSpan[], time: number | null | undefined): number {
  if (time == null || spans.length === 0) return -1;
  let lo = 0;
  let hi = spans.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    // A small lead so a seek to a span's own start lights that span.
    if (spans[mid].timestamp <= time + 0.05) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found;
}

/**
 * Saved transcripts load in pages. Playback (or a seek) past the end of the
 * loaded part needs the next page, or nothing is lit or followed there.
 */
export function needsMoreForPlayback(spans: readonly TimedSpan[], time: number | null | undefined, hasMore: boolean): boolean {
  if (time == null || !hasMore) return false;
  const last = spans[spans.length - 1];
  if (!last) return true;
  return time > (last.endTime ?? last.timestamp);
}
