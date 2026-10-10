// Locale-aware formatting of sizes, counts, durations and dates.
import { locale, t } from './i18n';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const;

/**
 * Decimal (SI) byte size, as drives are labeled: `64 GB`, `6.2 MB`.
 * `digits` decimals are shown unless the value is a whole number in its unit.
 */
export function formatBytes(bytes: number, digits = 1): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const whole = Number.isInteger(value) || unit === 0;
  const text = new Intl.NumberFormat(locale, {
    minimumFractionDigits: whole ? 0 : digits,
    maximumFractionDigits: whole ? 0 : digits,
  }).format(value);
  return `${text} ${UNITS[unit]}`;
}

export function formatCount(n: number): string {
  return new Intl.NumberFormat(locale).format(n);
}

/** Video length: `0:42`, `1:02:03`. */
export function formatClock(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  const ss = String(s).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

/** Elapsed time in words, in whole minutes (e.g. "11 min", "1 h 5 min"). */
export function formatElapsed(seconds: number): string {
  const minutes = Math.round(seconds / 60);
  if (minutes < 1) return t('duration.underMinute');
  if (minutes < 60) return t('duration.minutes', { minutes });
  return t('duration.hoursMinutes', { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
}

/** Parses an ISO 8601 local date-time (no zone) as local time. */
function parseLocal(iso: string): Date {
  return new Date(iso);
}

const dateTimeFormat = new Intl.DateTimeFormat(locale, {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
});
const longDateFormat = new Intl.DateTimeFormat(locale, { dateStyle: 'long' });

/** Numeric date and time without seconds (e.g. `2025/08/14 15:30`). */
export function formatDateTime(iso: string): string {
  return dateTimeFormat.format(parseLocal(iso));
}

/** Long localized date (e.g. "August 14, 2025"); takes a date-time or a `YYYY-MM-DD` day. */
export function formatLongDate(iso: string): string {
  return longDateFormat.format(parseLocal(iso.length === 10 ? `${iso}T00:00:00` : iso));
}
