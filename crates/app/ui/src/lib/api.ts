// Typed access to the Tauri commands and the helper event stream (see src/commands.rs).
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Destination } from './bindings/Destination';
import type { Drive } from './bindings/Drive';
import type { DriveId } from './bindings/DriveId';
import type { HelperError } from './bindings/HelperError';
import type { HelperEvent } from './bindings/HelperEvent';
import type { RecoveryId } from './bindings/RecoveryId';
import type { RecoveryRequest } from './bindings/RecoveryRequest';
import type { RepairRequest } from './bindings/RepairRequest';
import type { ScanId } from './bindings/ScanId';

/** Must match `HELPER_EVENT` in src/main.rs. */
export const HELPER_EVENT = 'helper-event';

export const api = {
  listDrives: () => invoke<Drive[]>('list_drives'),
  openImage: (path: string) => invoke<Drive>('open_image', { path }),
  listDestinations: (source: DriveId) => invoke<Destination[]>('list_destinations', { source }),
  /** Unless `fresh`, continues the drive's saved scan (see `Drive.savedScan`). */
  startScan: (drive: DriveId, fresh: boolean) => invoke<ScanId>('start_scan', { drive, fresh }),
  pauseScan: (scan: ScanId) => invoke<null>('pause_scan', { scan }),
  resumeScan: (scan: ScanId) => invoke<null>('resume_scan', { scan }),
  cancelScan: (scan: ScanId) => invoke<null>('cancel_scan', { scan }),
  recover: (request: RecoveryRequest) => invoke<RecoveryId>('recover', { request }),
  repair: (request: RepairRequest) => invoke<null>('repair', { request }),
};

export function onHelperEvent(handler: (event: HelperEvent) => void): Promise<UnlistenFn> {
  return listen<HelperEvent>(HELPER_EVENT, (e) => handler(e.payload));
}

export function isHelperError(value: unknown): value is HelperError {
  return typeof value === 'object' && value !== null && 'code' in value;
}
