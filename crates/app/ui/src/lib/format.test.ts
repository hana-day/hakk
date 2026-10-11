import { describe, expect, it } from 'vitest';
import { formatBytes, formatClock, formatCount, formatDateTime, formatElapsed, formatLongDate } from './format';
import { locale, parts, t } from './i18n';
import { ja } from './i18n/ja';

describe('format', () => {
  it('formats sizes like the mock screens', () => {
    expect(formatBytes(64e9)).toBe('64 GB');
    expect(formatBytes(2e12)).toBe('2 TB');
    expect(formatBytes(27.04e9)).toBe('27.0 GB');
    expect(formatBytes(6.2e6)).toBe('6.2 MB');
    expect(formatBytes(92.4e6, 0)).toBe('92 MB');
    expect(formatBytes(512)).toBe('512 B');
  });

  it('formats counts and clocks', () => {
    expect(formatCount(3412)).toBe('3,412');
    expect(formatClock(42)).toBe('0:42');
    expect(formatClock(135)).toBe('2:15');
    expect(formatClock(3723)).toBe('1:02:03');
  });

  it('picks the right elapsed-time message', () => {
    expect(formatElapsed(20)).toBe(t('duration.underMinute'));
    expect(formatElapsed(660)).toBe(t('duration.minutes', { minutes: 11 }));
    expect(formatElapsed(3900)).toBe(t('duration.hoursMinutes', { hours: 1, minutes: 5 }));
  });

  it('reads capture dates as local time', () => {
    const long = new Intl.DateTimeFormat(locale, { dateStyle: 'long' });
    expect(formatLongDate('2025-08-14T15:30:22')).toBe(long.format(new Date(2025, 7, 14)));
    expect(formatLongDate('2025-08-03')).toBe(long.format(new Date(2025, 7, 3)));
    expect(formatDateTime('2025-08-14T15:30:22')).toContain('15:30');
  });
});

describe('i18n', () => {
  it('fills placeholders and formats numbers', () => {
    expect(t('results.showNew', { count: 1200 })).toBe(ja.results.showNew.replace('{count}', '1,200'));
  });

  it('marks placeholders so they can be styled', () => {
    const p = parts('progress.remaining', { minutes: 7 });
    expect(p.filter((x) => x.param !== null)).toEqual([{ text: '7', param: 'minutes' }]);
    expect(p.map((x) => x.text).join('')).toBe(t('progress.remaining', { minutes: 7 }));
  });
});
