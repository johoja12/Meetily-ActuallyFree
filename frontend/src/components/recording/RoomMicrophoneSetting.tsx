'use client';

import { Switch } from '@/components/ui/switch';

export function RoomMicrophoneSetting({ enabled, disabled, onChange }: {
  enabled: boolean;
  disabled: boolean;
  onChange: (enabled: boolean) => void;
}) {
  return (
    <div className="space-y-2">
      <label className="flex items-center justify-between gap-4 text-sm font-medium text-af-text">
        Room microphone
        <Switch checked={enabled} disabled={disabled} onCheckedChange={onChange} aria-label="Room microphone" />
      </label>
      <p className="text-xs leading-relaxed text-af-text-3">
        Record yourself and nearby speakers, such as a Desk Pro, through one microphone.
        Computer audio is excluded. Speaker identification labels voices when available;
        otherwise speech is labeled Room. Choose before recording.
      </p>
    </div>
  );
}
