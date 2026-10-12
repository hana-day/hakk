<script lang="ts">
  import type { DiskMap } from '../../lib/bindings/DiskMap';
  import { t } from '../../lib/i18n';

  let {
    map,
    percent,
    errors,
    strip = false,
  }: {
    map: DiskMap;
    percent: number;
    errors: number;
    /** A band a few cells high, read left to right (fits on one line), legend as a tooltip. */
    strip?: boolean;
  } = $props();

  /** Rows of the band. */
  const ROWS = 6;

  // Only scans that read part of the disk have skipped cells; show the legend entry then.
  const hasSkipped = $derived(map.cells.includes('skipped'));
  const legend = $derived(
    [
      hasSkipped ? t('diskMap.skipped') : null,
      t('diskMap.unread'),
      t('diskMap.read'),
      t('diskMap.found'),
      t('diskMap.current'),
      t('diskMap.error', { count: errors }),
    ]
      .filter((x) => x !== null)
      .join(' / '),
  );
</script>

<div class="map-wrap" class:strip title={strip ? legend : undefined}>
  <div
    class="map"
    role="img"
    aria-label={t('diskMap.label', { percent })}
    style:grid-template-columns={strip ? `repeat(${Math.ceil(map.cells.length / ROWS)}, 5px)` : undefined}
  >
    {#each map.cells as cell, i (i)}
      <span class="cell {cell}" class:current={i === map.current}></span>
    {/each}
  </div>
  {#if !strip}
  <div class="legend">
    {#if hasSkipped}<span><i class="cell skipped"></i>{t('diskMap.skipped')}</span>{/if}
    <span><i class="cell unread"></i>{t('diskMap.unread')}</span>
    <span><i class="cell read"></i>{t('diskMap.read')}</span>
    <span><i class="cell found"></i>{t('diskMap.found')}</span>
    <span><i class="cell current"></i>{t('diskMap.current')}</span>
    <span><i class="cell error"></i>{t('diskMap.error', { count: errors })}</span>
  </div>
  {/if}
</div>

<style>
  .map-wrap { flex: 1 1 520px; max-width: 720px; min-width: 0; display: flex; flex-direction: column; gap: 8px; }
  .map { display: grid; grid-template-columns: repeat(48, minmax(0, 1fr)); gap: 2px; }
  .map .cell { display: block; aspect-ratio: 1; }
  .cell { border-radius: 2px; background: var(--track); }
  .cell.skipped { background: transparent; box-shadow: inset 0 0 0 1px var(--track); }
  .cell.read { background: var(--map-read); }
  .cell.found { background: var(--brand); }
  .cell.error { background: var(--error); }
  .cell.current { background: var(--text); }
  /* Read in disk order left to right: cells fill each column top to bottom. */
  .strip { flex: 0 0 auto; max-width: none; gap: 0; }
  .strip .map { grid-auto-flow: column; grid-template-rows: repeat(6, 5px); gap: 1px; }
  .strip .map .cell { width: 5px; height: 5px; aspect-ratio: auto; border-radius: 1px; }
  .legend { display: flex; flex-wrap: wrap; gap: 6px 16px; font-size: 12px; color: var(--text-3); }
  .legend span { display: inline-flex; align-items: center; gap: 6px; }
  .legend i { display: inline-block; width: 10px; height: 10px; }
</style>
