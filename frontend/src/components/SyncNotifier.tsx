'use client';

import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { announceChange } from '@/lib/workspace-api';
import { VOICE_PROFILES_CHANGED_EVENT } from '@/lib/voice-profiles';
import { announceVoiceMatchChanges } from '@/lib/voice-matches';
import { SYNC_IMPORTED_EVENT, type SyncImported } from '@/lib/sync';

/**
 * Refreshes open pages when library sync brings in another computer's
 * changes. The native worker has already written them; this only tells the
 * UI which lists and meetings to reload.
 */
export function SyncNotifier() {
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    listen<SyncImported>(SYNC_IMPORTED_EVENT, ({ payload }) => {
      if (payload.meeting_ids.length > 0) {
        announceChange('meetings');
        announceChange('notes');
        announceChange('actions');
        announceVoiceMatchChanges(payload.meeting_ids);
      }
      if (payload.library || payload.meeting_ids.length > 0) {
        announceChange('people');
        announceChange('groups');
      }
      if (payload.voices) window.dispatchEvent(new Event(VOICE_PROFILES_CHANGED_EVENT));
    })
      .then((unlisten) => (disposed ? unlisten() : (stop = unlisten)))
      .catch(() => undefined);
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  return null;
}
