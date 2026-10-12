// Thumbnails fetched from the helper on demand: a cache, and a small queue so that only a
// couple are made at a time (each one reads the drive and runs a decoder).
import { SvelteMap } from 'svelte/reactivity';
import { api } from './api';
import type { ScanId } from './bindings/ScanId';

/** Thumbnails being made at once. */
const MAX_RUNNING = 2;

/** `data:` URL per key, or null when no picture can be made. */
const cache = new SvelteMap<string, string | null>();
const queue: { key: string; scan: ScanId; file: number; size: number }[] = [];
const queued = new Set<string>();
let running = 0;

function key(scan: ScanId, file: number, size: number): string {
  return `${scan}:${file}:${size}`;
}

/** The thumbnail if made: a URL, null if it cannot be made, undefined if not yet. */
export function thumbnail(scan: ScanId, file: number, size: number): string | null | undefined {
  return cache.get(key(scan, file, size));
}

/** Asks for a thumbnail. The latest requests are served first (what just scrolled in). */
export function request(scan: ScanId, file: number, size: number): void {
  const k = key(scan, file, size);
  if (cache.has(k) || queued.has(k)) return;
  queued.add(k);
  queue.push({ key: k, scan, file, size });
  pump();
}

/** Forgets a request that has not started (the item scrolled out of view). */
export function cancel(scan: ScanId, file: number, size: number): void {
  const k = key(scan, file, size);
  const i = queue.findIndex((j) => j.key === k);
  if (i >= 0) {
    queue.splice(i, 1);
    queued.delete(k);
  }
}

/** Drops every thumbnail (a new scan reuses file ids). */
export function clearThumbnails(): void {
  cache.clear();
  queue.length = 0;
  queued.clear();
}

function pump(): void {
  while (running < MAX_RUNNING && queue.length > 0) {
    const job = queue.pop();
    if (job === undefined) return;
    running += 1;
    api
      .thumbnail(job.scan, job.file, job.size)
      .then((t) => cache.set(job.key, `data:${t.mime};base64,${t.data}`))
      .catch(() => cache.set(job.key, null))
      .finally(() => {
        queued.delete(job.key);
        running -= 1;
        pump();
      });
  }
}
