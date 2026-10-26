/**
 * Wording for library sync status. Kept free of Tauri imports so it can be
 * tested under node.
 */
export interface SyncDevice {
  id: string;
  name: string;
  platform: string;
  app_version: string;
  /** When that computer last wrote to the library (RFC 3339). */
  updated: string;
}

function since(iso: string | null, now: Date): string | null {
  if (!iso) return null;
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return null;
  const seconds = Math.max(0, (now.getTime() - then.getTime()) / 1000);
  if (seconds < 60) return 'just now';
  if (seconds < 60 * 60) return `${Math.floor(seconds / 60)} min ago`;
  if (seconds < 60 * 60 * 24) return `${Math.floor(seconds / 3600)} h ago`;
  return `on ${then.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })}`;
}

/** "Synced 5 min ago" */
export function describeLastSync(iso: string | null, now = new Date()): string {
  const when = since(iso, now);
  return when ? `Synced ${when}` : 'Not synced yet';
}

/** "Also used on MacBook Air (synced 2 min ago)." */
export function describeDevices(devices: SyncDevice[], now = new Date()): string {
  if (devices.length === 0) return 'No other computer has joined this library yet.';
  const parts = devices.map((device) => {
    const when = since(device.updated, now);
    return when ? `${device.name} (synced ${when})` : device.name;
  });
  const list = parts.length === 1 ? parts[0] : `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
  return `Also used on ${list}.`;
}
