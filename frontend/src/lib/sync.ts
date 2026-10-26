/**
 * Library sync through a shared folder (docs/LIBRARY_SYNC.md). The native
 * worker in `src-tauri/src/sync/` does the work; this module only reads its
 * status, changes its settings, and names its events.
 */
import { invoke } from '@tauri-apps/api/core';
import type { SyncDevice } from './sync-status';

export type { SyncDevice } from './sync-status';

export interface SyncStatus {
  enabled: boolean;
  library_root: string | null;
  device_id: string;
  device_name: string;
  running: boolean;
  last_sync: string | null;
  last_error: string | null;
  devices: SyncDevice[];
}

export interface SyncImported {
  meeting_ids: string[];
  library: boolean;
  voices: boolean;
}

export const SYNC_STATUS_EVENT = 'sync-status';
export const SYNC_IMPORTED_EVENT = 'sync-imported';

export const getSyncStatus = () => invoke<SyncStatus>('get_sync_status');
export const enableSync = (libraryRoot: string, deviceName?: string) =>
  invoke<SyncStatus>('enable_sync', { libraryRoot, deviceName: deviceName ?? null });
export const disableSync = () => invoke<SyncStatus>('disable_sync');
export const syncNow = () => invoke<void>('sync_now');
export const setSyncDeviceName = (name: string) => invoke<SyncStatus>('set_sync_device_name', { name });
