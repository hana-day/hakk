// State of one scan session: everything the helper has reported, plus the user's selection.
import { SvelteSet } from 'svelte/reactivity';
import { api } from './api';
import type { Destination } from './bindings/Destination';
import type { Drive } from './bindings/Drive';
import type { FoundFile } from './bindings/FoundFile';
import type { HelperEvent } from './bindings/HelperEvent';
import type { RecoveryProgress } from './bindings/RecoveryProgress';
import type { RecoveryReport } from './bindings/RecoveryReport';
import type { RepairMethod } from './bindings/RepairMethod';
import type { RepairReport } from './bindings/RepairReport';
import type { ScanId } from './bindings/ScanId';
import type { ScanProgress } from './bindings/ScanProgress';
import type { ScanSummary } from './bindings/ScanSummary';

export class ScanSession {
  readonly drive: Drive;
  readonly scanId: ScanId;

  progress = $state<ScanProgress | null>(null);
  summary = $state<ScanSummary | null>(null);
  recovery = $state<RecoveryProgress | null>(null);
  recoveryReport = $state<RecoveryReport | null>(null);
  lastRepair = $state<RepairReport | null>(null);
  destinations = $state<Destination[]>([]);
  destination = $state<Destination | null>(null);
  /** A pause or resume was asked for and the scan has not reported it yet (finishing the
   *  block in hand and saving can take a moment). */
  switching = $state<'pausing' | 'resuming' | null>(null);
  readonly selection = new SvelteSet<number>();
  readonly repairing = new SvelteSet<number>();

  // Files live in plain collections for speed; `rev` is bumped whenever they change so
  // derived values recompute once per event instead of once per file.
  private rev = $state(0);
  private readonly files = new Map<number, FoundFile>();
  /** Files the results list shows. New files wait outside it until the user asks for them,
   *  so the list does not reshuffle under the pointer while scanning. */
  private readonly shown = new Set<number>();

  constructor(drive: Drive, scanId: ScanId) {
    this.drive = drive;
    this.scanId = scanId;
  }

  readonly scanning = $derived(this.summary === null);

  /** All files found so far, shown or not (for counts). */
  readonly allFiles = $derived.by(() => {
    void this.rev;
    return [...this.files.values()];
  });

  /** Files accepted into the results list. */
  readonly shownFiles = $derived.by(() => {
    void this.rev;
    return this.allFiles.filter((f) => this.shown.has(f.id));
  });

  readonly pendingFiles = $derived.by(() => {
    void this.rev;
    return this.allFiles.filter((f) => !this.shown.has(f.id));
  });

  readonly selectedSize = $derived.by(() => {
    void this.rev;
    let size = 0;
    for (const id of this.selection) size += this.files.get(id)?.size ?? 0;
    return size;
  });

  file(id: number): FoundFile | undefined {
    return this.files.get(id);
  }

  async loadDestinations(): Promise<void> {
    this.destinations = await api.listDestinations(this.drive.id);
    this.destination ??= this.destinations.find((d) => d.blocked === null) ?? null;
  }

  /** Moves every pending file into the results list. */
  showPending(): void {
    for (const id of this.files.keys()) this.shown.add(id);
    this.rev += 1;
  }

  handle(event: HelperEvent): void {
    switch (event.type) {
      case 'scanProgress':
        if (event.scanId !== this.scanId) return;
        this.progress = event;
        if (
          (this.switching === 'pausing' && event.state !== 'running') ||
          (this.switching === 'resuming' && event.state === 'running')
        ) {
          this.switching = null;
        }
        return;
      case 'filesFound': {
        if (event.scanId !== this.scanId) return;
        // Show the first files right away; later ones wait for the user.
        const showNow = this.shown.size === 0;
        for (const file of event.files) {
          this.files.set(file.id, file);
          if (showNow) this.shown.add(file.id);
        }
        this.rev += 1;
        return;
      }
      case 'filesUpdated':
        if (event.scanId !== this.scanId) return;
        for (const u of event.updates) {
          const file = this.files.get(u.id);
          if (file) this.files.set(u.id, { ...file, status: u.status, repair: u.repair });
        }
        this.rev += 1;
        return;
      case 'filesMerged':
        if (event.scanId !== this.scanId) return;
        for (const { from, into } of event.merges) {
          this.files.delete(from);
          this.shown.delete(from);
          if (this.selection.delete(from)) this.selection.add(into);
        }
        this.rev += 1;
        return;
      case 'scanFinished':
        if (event.scanId !== this.scanId) return;
        this.summary = event;
        // Results no longer grow, so there is nothing left to hold back.
        this.showPending();
        return;
      case 'recoveryProgress':
        this.recovery = event;
        return;
      case 'recoveryFinished':
        this.recovery = null;
        this.recoveryReport = event;
        this.selection.clear();
        return;
      case 'repairFinished':
        this.repairing.delete(event.file);
        this.lastRepair = event;
        return;
    }
  }

  async pause(): Promise<void> {
    this.switching = 'pausing';
    try {
      await api.pauseScan(this.scanId);
    } catch (err) {
      this.switching = null;
      throw err;
    }
  }

  async resume(): Promise<void> {
    this.switching = 'resuming';
    try {
      await api.resumeScan(this.scanId);
    } catch (err) {
      this.switching = null;
      throw err;
    }
  }

  async cancel(): Promise<void> {
    if (this.scanning) await api.cancelScan(this.scanId);
  }

  async recover(): Promise<void> {
    await this.recoverFiles([...this.selection]);
  }

  /** Recovers these files (not the selection) to the chosen destination. */
  async recoverFiles(ids: number[]): Promise<void> {
    if (this.destination === null) return;
    const files = ids.filter((id) => this.files.has(id));
    await api.recover({ scanId: this.scanId, files, destination: this.destination.id });
  }

  async repair(id: number, method: RepairMethod): Promise<void> {
    if (this.destination === null) return;
    this.repairing.add(id);
    try {
      await api.repair({ scanId: this.scanId, file: id, method, destination: this.destination.id });
    } catch (err) {
      this.repairing.delete(id);
      throw err;
    }
  }
}
