'use client';

/**
 * Settings > Sync: keep this library in step with other computers through a
 * folder that Google Drive (or Dropbox, OneDrive, iCloud Drive) syncs.
 */
import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { Cloud, FolderOpen, RefreshCw } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Spinner } from '@/components/ui/spinner';
import { describeDevices, describeLastSync } from '@/lib/sync-status';
import {
  disableSync,
  enableSync,
  getSyncStatus,
  setSyncDeviceName,
  SYNC_STATUS_EVENT,
  syncNow,
  type SyncStatus,
} from '@/lib/sync';

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function SyncSettings() {
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    getSyncStatus()
      .then((next) => {
        if (disposed) return;
        setStatus(next);
        setName(next.device_name);
      })
      .catch(() => undefined);
    listen<SyncStatus>(SYNC_STATUS_EVENT, ({ payload }) => setStatus(payload))
      .then((unlisten) => (disposed ? unlisten() : (stop = unlisten)))
      .catch(() => undefined);
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  const chooseFolder = useCallback(async () => {
    const picked = await open({ directory: true, multiple: false, title: 'Choose the Meetily library folder' });
    if (typeof picked !== 'string') return;
    setBusy(true);
    try {
      const next = await enableSync(picked, name);
      setStatus(next);
      toast.success('Sync is on', { description: 'This computer’s meetings are being added to the library.' });
    } catch (error) {
      toast.error('Could not use that folder', { description: message(error) });
    } finally {
      setBusy(false);
    }
  }, [name]);

  const turnOff = useCallback(async () => {
    try {
      setStatus(await disableSync());
      toast.success('Sync is off', { description: 'Your meetings stay on this computer.' });
    } catch (error) {
      toast.error('Could not turn off sync', { description: message(error) });
    }
  }, []);

  const saveName = useCallback(async () => {
    if (!status || name.trim() === status.device_name) return;
    try {
      setStatus(await setSyncDeviceName(name));
    } catch (error) {
      toast.error('Could not rename this computer', { description: message(error) });
    }
  }, [name, status]);

  if (!status) return <div className="af-skeleton h-40 rounded-2xl" />;

  return (
    <div className="space-y-6">
      <div className="rounded-2xl border border-af-border bg-af-panel-2/40 p-5">
        <h3 className="mb-2 flex items-center gap-2 text-[15px] font-semibold text-af-text">
          <Cloud className="h-5 w-5 text-af-accent" />
          Sync with other computers
        </h3>
        <p className="text-sm text-af-text-2">
          Meetings, transcripts, speaker names, contacts, notes, summaries, action items, voices and recordings are kept
          in step through a folder your cloud drive syncs. Each computer keeps its own copy and works offline.
        </p>

        <label className="mt-4 block text-xs font-medium text-af-text-2" htmlFor="sync-device-name">
          This computer
        </label>
        <Input
          id="sync-device-name"
          className="mt-1 max-w-xs"
          value={name}
          onChange={(event) => setName(event.target.value)}
          onBlur={() => void saveName()}
        />

        {status.enabled && status.library_root ? (
          <div className="mt-4 space-y-3">
            <div className="rounded-xl border border-af-border bg-af-panel px-3 py-2">
              <div className="text-[11px] uppercase tracking-wide text-af-text-4">Library folder</div>
              <div className="break-all text-[13px] text-af-text">{status.library_root}</div>
            </div>
            <p className="flex items-center gap-2 text-[13px] text-af-text-2" role="status">
              {status.running ? <Spinner size={12} /> : null}
              {status.running ? 'Syncing…' : describeLastSync(status.last_sync)}
            </p>
            {status.last_error ? (
              <p className="rounded-lg border border-af-warning/30 bg-af-warning/[0.08] px-3 py-2 text-xs text-af-text-2">
                {status.last_error}
              </p>
            ) : null}
            <p className="text-[13px] text-af-text-3">{describeDevices(status.devices)}</p>
            <div className="flex flex-wrap gap-2">
              <Button variant="secondary" size="sm" onClick={() => void syncNow()} disabled={status.running}>
                <RefreshCw className="mr-1.5 h-3.5 w-3.5" /> Sync now
              </Button>
              <Button variant="outline" size="sm" onClick={() => void chooseFolder()} disabled={busy}>
                <FolderOpen className="mr-1.5 h-3.5 w-3.5" /> Change folder
              </Button>
              <Button variant="ghost" size="sm" onClick={() => void turnOff()}>
                Turn off sync
              </Button>
            </div>
          </div>
        ) : (
          <div className="mt-4">
            <Button onClick={() => void chooseFolder()} loading={busy}>
              <FolderOpen className="mr-1.5 h-4 w-4" /> Choose library folder
            </Button>
            <p className="mt-2 text-xs text-af-text-3">
              Pick or create a folder inside Google Drive (for example <span className="font-mono">My Drive/Meetily Library</span>).
              On your other computer, choose the same folder. Meetings already on each computer are merged; nothing is deleted.
            </p>
          </div>
        )}
      </div>

      <div className="rounded-2xl border border-af-border p-5 text-xs leading-relaxed text-af-text-3">
        <p className="font-medium text-af-text-2">Good to know</p>
        <ul className="mt-1 list-disc space-y-1 pl-4">
          <li>In Google Drive for desktop, set this folder to <em>Available offline</em> (mirror) so recordings play without waiting to download.</li>
          <li>Recordings move into the library a couple of minutes after a meeting is saved; recording itself always stays on this computer.</li>
          <li>Models, settings and API keys are not synced. Download models on each computer.</li>
          <li>If the same thing is edited on two computers at once, the later edit wins.</li>
        </ul>
      </div>
    </div>
  );
}
