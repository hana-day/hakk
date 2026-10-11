// Mapping of protocol values to what the UI shows.
import type { Drive } from './bindings/Drive';
import type { DriveKind } from './bindings/DriveKind';
import type { FileStatus } from './bindings/FileStatus';
import type { FoundFile } from './bindings/FoundFile';
import { isHelperError } from './api';
import { formatBytes } from './format';
import { t } from './i18n';

/** Drive name with its kind and capacity, e.g. `EOS_DIGITAL (SD card, 64 GB)`. */
export function driveTitle(drive: Drive): string {
  return t('header.drive', {
    name: drive.name,
    kind: t(`driveKind.${drive.kind}`),
    size: formatBytes(drive.capacityBytes),
  });
}

export function driveIcon(kind: DriveKind): 'sd' | 'hdd' | 'internal' {
  switch (kind) {
    case 'sdCard':
    case 'usbFlash':
      return 'sd';
    case 'internalHdd':
    case 'internalSsd':
      return 'internal';
    default:
      return 'hdd';
  }
}

export function badgeClass(status: FileStatus): string {
  switch (status) {
    case 'recoverable':
    case 'repaired':
      return 'badge badge-ok';
    case 'partiallyDamaged':
    case 'indexMissing':
      return 'badge badge-warn';
    case 'unknown':
      return 'badge badge-neutral';
  }
}

/** Badge for a file: its verification status, or a warning while an unverified file
 *  probably lies in overwritten space. */
export function statusBadge(file: FoundFile): { text: string; className: string } {
  if (file.status === 'unknown' && file.confidence === 'low') {
    return { text: t('status.likelyOverwritten'), className: 'badge badge-warn' };
  }
  return { text: t(`status.${file.status}`), className: badgeClass(file.status) };
}

/** File name, marked when part of it was guessed. */
export function displayName(file: FoundFile): string {
  const guessed = file.origin.type === 'fileSystem' && !file.origin.nameExact;
  return guessed ? t('results.guessedName', { name: file.name }) : file.name;
}

// Placeholder thumbnail colors until real previews exist.
const TONES = ['#8FA3B8', '#C9B79C', '#7E9C84', '#B08F8F', '#9AA6C4', '#D2C1A4', '#6F8796', '#A7B39A'];

export function tone(id: number): string {
  return TONES[id % TONES.length] ?? '#8FA3B8';
}

/** User-facing text for a failed request. */
export function errorText(err: unknown): string {
  if (isHelperError(err)) return t(`errors.${err.code}`);
  return t('errors.internal');
}
