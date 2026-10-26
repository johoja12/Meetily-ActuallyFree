'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import type { RecordingPreferences } from '@/components/RecordingSettings';

const changed = 'room-microphone-preference-changed';

export function useRoomMicrophone(saveQueue?: { current: Promise<void> }) {
  const [enabled, setEnabled] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState(false);

  useEffect(() => {
    let disposed = false;
    const refresh = () => {
      void invoke<RecordingPreferences>('get_recording_preferences').then((prefs) => {
        if (!disposed) { setEnabled(!!prefs.room_microphone); setLoadError(false); }
      }).catch(() => {
        if (!disposed) {
          setLoadError(true);
          toast.error('Could not load microphone mode', {
            id: 'room-microphone-load',
            description: 'Open microphone settings and choose a mode to retry.',
          });
        }
      })
        .finally(() => { if (!disposed) setLoading(false); });
    };
    refresh();
    window.addEventListener(changed, refresh);
    return () => { disposed = true; window.removeEventListener(changed, refresh); };
  }, []);

  const update = useCallback(async (room: boolean) => {
    setSaving(true);
    try {
      const save = async () => {
        const prefs = await invoke<RecordingPreferences>('get_recording_preferences');
        await invoke('set_recording_preferences', { preferences: { ...prefs, room_microphone: room } });
      };
      // Device and gain writes on the record card use read/modify/write too.
      // Share their queue so a slow device save cannot restore the old mode.
      if (saveQueue) {
        const pending = saveQueue.current.then(save);
        saveQueue.current = pending.catch(() => undefined);
        await pending;
      } else {
        await save();
      }
      setEnabled(room);
      setLoadError(false);
      window.dispatchEvent(new Event(changed));
    } catch (error) {
      toast.error('Could not save microphone mode', { description: String(error) });
    } finally { setSaving(false); }
  }, [saveQueue]);

  return { enabled, loading, saving, loadError, update };
}
