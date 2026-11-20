import { useEffect, useRef, useState } from 'react';
import { useRecordingState } from '@/contexts/RecordingStateContext';

interface UseRecordingStateSyncReturn {
  isBackendRecording: boolean;
  isRecordingDisabled: boolean;
  setIsRecordingDisabled: (value: boolean) => void;
}

/**
 * Keeps the recorder page's local `isRecording` flag in step with native
 * recording state, and owns the "recording disabled" flag used while a stop
 * is being processed.
 *
 * Native state comes from RecordingStateContext, which follows the
 * recording-started/stopped events, syncs once on mount (a reload while
 * recording), and polls while recording. This hook used to poll
 * `is_recording` every second itself, even with nothing recording (issue
 * #21). It reacts only to changes of the native flag: the page sets its own
 * flag optimistically before the native start event arrives, and comparing
 * levels on every render would undo that.
 */
export function useRecordingStateSync(
  isRecording: boolean,
  setIsRecording: (value: boolean) => void,
  setIsMeetingActive: (value: boolean) => void
): UseRecordingStateSyncReturn {
  const [isRecordingDisabled, setIsRecordingDisabled] = useState(false);
  const { isRecording: nativeRecording } = useRecordingState();
  // Starts false so a page mounted mid-recording (navigating back to the
  // recorder) still picks the running recording up.
  const previous = useRef(false);

  useEffect(() => {
    if (previous.current === nativeRecording) return;
    previous.current = nativeRecording;
    if (nativeRecording && !isRecording) {
      setIsRecording(true);
      setIsMeetingActive(true);
    } else if (!nativeRecording && isRecording) {
      setIsRecording(false);
    }
  }, [nativeRecording, isRecording, setIsRecording, setIsMeetingActive]);

  return {
    isBackendRecording: isRecording,
    isRecordingDisabled,
    setIsRecordingDisabled,
  };
}
