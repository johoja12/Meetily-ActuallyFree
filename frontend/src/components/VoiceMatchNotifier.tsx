'use client';

import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { announceChange } from '@/lib/workspace-api';
import { VOICE_PROFILES_CHANGED_EVENT } from '@/lib/voice-profiles';
import { describeMatchedMeetings, describeVoiceMatches } from '@/lib/voice-match-summary';
import {
  announceVoiceMatchChanges,
  NATIVE_VOICE_PROFILES_CHANGED_EVENT,
  undoVoiceMatches,
  VOICE_MATCHES_APPLIED_EVENT,
  type VoiceMatch,
} from '@/lib/voice-matches';

/**
 * Reports names the native voice worker gave to unnamed speakers, from any
 * page, with an Undo. The worker owns the renames; this only tells open pages
 * to reload and offers the undo.
 */
export function VoiceMatchNotifier() {
  useEffect(() => {
    let disposed = false;
    const stops: Array<() => void> = [];
    const keep = (stop: () => void) => (disposed ? stop() : stops.push(stop));

    listen<{ matches: VoiceMatch[] }>(VOICE_MATCHES_APPLIED_EVENT, ({ payload }) => {
      const matches = payload.matches ?? [];
      if (matches.length === 0) return;
      announceVoiceMatchChanges(matches.map((match) => match.meeting_id));
      announceChange('people');
      toast.success(describeVoiceMatches(matches), {
        description: describeMatchedMeetings(matches),
        duration: 12_000,
        action: {
          label: 'Undo',
          onClick: () => {
            undoVoiceMatches(matches)
              .then((meetingIds) => {
                announceVoiceMatchChanges(meetingIds);
                announceChange('people');
                toast.success('Names removed', { description: 'Those speakers will not be named that way again.' });
              })
              .catch((error) =>
                toast.error('Could not undo the names', { description: error instanceof Error ? error.message : String(error) }),
              );
          },
        },
      });
    }).then(keep).catch(() => {});

    listen(NATIVE_VOICE_PROFILES_CHANGED_EVENT, () => {
      window.dispatchEvent(new Event(VOICE_PROFILES_CHANGED_EVENT));
    }).then(keep).catch(() => {});

    return () => {
      disposed = true;
      stops.forEach((stop) => stop());
    };
  }, []);

  return null;
}
