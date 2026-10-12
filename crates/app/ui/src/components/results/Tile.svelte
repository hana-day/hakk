<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import type { ScanId } from '../../lib/bindings/ScanId';
  import { displayName, statusBadge } from '../../lib/display';
  import { formatBytes, formatClock } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';
  import Thumb from './Thumb.svelte';

  let {
    scanId,
    file,
    selected,
    previewed,
    ontoggle,
    onpreview,
  }: {
    scanId: ScanId;
    file: FoundFile;
    selected: boolean;
    previewed: boolean;
    ontoggle: (id: number) => void;
    onpreview: (id: number) => void;
  } = $props();

  const badge = $derived(statusBadge(file));
</script>

<div class="tile" class:on={selected} class:previewed>
  <button type="button" class="open" onclick={() => onpreview(file.id)} aria-label={displayName(file)}>
    <span class="thumb" class:partial={file.status === 'partiallyDamaged'}>
      <Thumb {scanId} {file} size={256} />
      {#if file.kind === 'video'}
        <span class="duration num">
          <Icon name="play" size={10} stroke={0} />{file.durationSeconds === null ? '' : formatClock(file.durationSeconds)}
        </span>
      {/if}
      {#if file.deleted}<span class="deleted">{t('deleted.deleted')}</span>{/if}
    </span>
    <span class="meta">
      <span class="name" title={displayName(file)}>{displayName(file)}</span>
      <span class="row">
        <span class={badge.className}>{badge.text}</span>
        <span class="num size">{formatBytes(file.size)}</span>
      </span>
    </span>
  </button>
  <button
    type="button"
    class="check"
    class:checked={selected}
    aria-pressed={selected}
    aria-label={t('list.select', { name: file.name })}
    onclick={() => ontoggle(file.id)}
  >
    {#if selected}<Icon name="check" size={12} stroke={3.5} />{/if}
  </button>
</div>

<style>
  .tile {
    position: relative;
    margin: 1px;
    overflow: hidden;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: var(--surface);
  }
  .tile.on { margin: 0; border: 2px solid var(--brand); box-shadow: 0 0 0 3px var(--brand-soft); }
  .tile.previewed:not(.on) { border-color: var(--text-3); }
  .open {
    display: flex;
    flex-direction: column;
    width: 100%;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
    cursor: pointer;
  }
  .thumb { position: relative; display: block; width: 100%; aspect-ratio: 4 / 3; }
  .thumb.partial::after {
    content: '';
    position: absolute;
    inset: 62% 0 0 0;
    background: rgba(110, 115, 124, 0.85);
  }
  .duration {
    position: absolute;
    left: 8px;
    bottom: 8px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 10px;
    background: rgba(21, 24, 29, 0.78);
    color: #fff;
    font-size: 12px;
  }
  .duration :global(svg) { fill: currentColor; }
  .deleted {
    position: absolute;
    left: 8px;
    top: 8px;
    padding: 1px 7px;
    border-radius: 8px;
    background: rgba(178, 64, 43, 0.85);
    color: #fff;
    font-size: 11px;
  }
  .check {
    position: absolute;
    top: 8px;
    right: 8px;
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: 50%;
    border: 2px solid #fff;
    background: rgba(21, 24, 29, 0.35);
    color: #fff;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
  }
  .check.checked { background: var(--brand); }
  .meta { display: flex; flex-direction: column; gap: 6px; padding: 10px 10px 12px; min-width: 0; }
  .name { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 6px; }
  .size { font-size: 11px; color: var(--text-3); white-space: nowrap; }
</style>
