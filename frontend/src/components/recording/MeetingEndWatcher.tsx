'use client';
import { useEffect, useRef } from 'react';
import { useRouter } from 'next/navigation';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import { requestRecordingStop } from '@/lib/recording-launch';
import { readLiveTitle } from '@/lib/live-session';
import { loadLabsPreferences } from '@/lib/labs';
import { automatedRecording } from '@/lib/meeting-automation';
import {
  endReason,
  freshEndState,
  isGoodbye,
  noteGoodbye,
  noteLevel,
  readMeetingEndPrefs,
  snooze,
  type EndReason,
  type EndState,
} from '@/lib/meeting-end';

const TOAST_ID = 'meeting-end';
const AUTO_STOP_MS = 60_000;

function promptText(reason: EndReason): string {
  if (reason.reason === 'app') return `${reason.app} call ended`;
  if (reason.reason === 'silence') return `No one has spoken for ${Math.max(1, Math.round(reason.quietMs / 60_000))} min`;
  return 'Sounds like the meeting is wrapping up';
}

/**
 * Mounted once in the main window. Watches the running recording for the
 * meeting ending (see lib/meeting-end.ts) and asks whether to stop. Stopping
 * goes through the normal stop-and-save path. Native capture never depends on
 * this window: closing it only loses the prompt.
 */
export function MeetingEndWatcher({ enabled }: { enabled: boolean }) {
  const router = useRouter();
  const routerRef = useRef(router);
  routerRef.current = router;

  useEffect(() => {
    if (!enabled) return;
    let state: EndState | null = null;
    let prompt: { autoStopAt: number | null; handled: boolean } | null = null;
    let disposed = false;
    const unlisteners: Promise<UnlistenFn>[] = [];

    const begin = () => {
      state = freshEndState(Date.now(), null);
      prompt = null;
      toast.dismiss(TOAST_ID);
    };
    const end = () => {
      state = null;
      prompt = null;
      toast.dismiss(TOAST_ID);
    };
    const stop = () => {
      if (prompt) prompt.handled = true;
      prompt = null;
      toast.dismiss(TOAST_ID);
      requestRecordingStop((href) => routerRef.current.push(href));
    };
    const keep = () => {
      if (prompt) prompt.handled = true;
      prompt = null;
      if (state) state = snooze(state, Date.now());
      toast.dismiss(TOAST_ID);
    };

    const show = (reason: EndReason, autoStop: boolean) => {
      const current = { autoStopAt: autoStop ? Date.now() + AUTO_STOP_MS : null, handled: false };
      prompt = current;
      const title = promptText(reason);
      toast(title, {
        id: TOAST_ID,
        description: autoStop ? 'Stopping and saving in 1 minute unless you keep recording.' : 'Stop and save the recording?',
        duration: Infinity,
        action: { label: 'Stop & save', onClick: stop },
        cancel: { label: 'Keep recording', onClick: keep },
        // Closing the prompt means keep recording, for a while.
        onDismiss: () => { if (!current.handled && prompt === current) keep(); },
      });
      invoke('show_simple_notification', { title, body: 'Open Meetily to stop and save the recording.' }).catch(() => {});
    };

    const tick = async () => {
      if (!state || disposed) return;
      try {
        const recording = await invoke<{ is_recording?: boolean; is_paused?: boolean }>('get_recording_state');
        if (!recording.is_recording) return end();
        // A paused recording is not silent; restart the quiet clock.
        if (recording.is_paused && state) state = { ...state, lastSoundAt: Date.now() };
      } catch { /* keep the last known state */ }
      if (!state) return;
      const now = Date.now();
      state = { ...state, calendarEndMs: readLiveTitle()?.calendarEvent?.end_ms ?? null };
      if (prompt) {
        if (prompt.autoStopAt != null && now >= prompt.autoStopAt) stop();
        return;
      }
      const prefs = readMeetingEndPrefs();
      if (!prefs.prompt) return;
      const reason = endReason(state, now);
      if (reason) show(reason, prefs.autoStop);
    };

    unlisteners.push(listen('recording-started', begin));
    unlisteners.push(listen('recording-stopped', end));
    unlisteners.push(listen<{ source: string; rms: number }>('recording-audio-levels', (event) => {
      if (state) state = noteLevel(state, event.payload.rms, Date.now());
    }));
    unlisteners.push(listen<{ text: string; is_partial?: boolean }>('transcript-update', (event) => {
      if (state && !event.payload.is_partial && isGoodbye(event.payload.text)) state = noteGoodbye(state, Date.now());
    }));
    unlisteners.push(listen<{ app: string; process: string }>('meeting-ended', (event) => {
      // Meeting automation stops the recording it started itself.
      const owned = automatedRecording();
      if (owned && owned.process === event.payload.process && loadLabsPreferences().meetingAutomation) return;
      if (state) state = { ...state, appEnded: { at: Date.now(), app: event.payload.app } };
    }));

    // A recording already running when the window loads is watched from now.
    invoke<{ is_recording?: boolean }>('get_recording_state')
      .then((recording) => { if (!disposed && recording.is_recording && !state) begin(); })
      .catch(() => {});
    const timer = window.setInterval(() => { void tick(); }, 2_000);

    return () => {
      disposed = true;
      window.clearInterval(timer);
      for (const unlisten of unlisteners) void unlisten.then((fn) => fn()).catch(() => {});
      toast.dismiss(TOAST_ID);
    };
  }, [enabled]);

  return null;
}
