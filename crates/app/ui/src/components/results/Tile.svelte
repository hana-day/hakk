<script lang="ts">
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import { badgeClass, tone } from '../../lib/display';
  import { formatBytes, formatClock } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';

  let {
    file,
    selected,
    ontoggle,
  }: { file: FoundFile; selected: boolean; ontoggle: (id: number) => void } = $props();
</script>

<button type="button" class="tile" class:on={selected} aria-pressed={selected} onclick={() => ontoggle(file.id)}>
  <span class="thumb" class:partial={file.status === 'partiallyDamaged'} style:background-color={tone(file.id)}>
    {#if file.kind === 'video'}
      <span class="duration num">
        <Icon name="play" size={10} stroke={0} />{file.durationSeconds === null ? '' : formatClock(file.durationSeconds)}
      </span>
    {/if}
    {#if selected}
      <span class="check"><Icon name="check" size={12} stroke={3.5} /></span>
    {/if}
  </span>
  <span class="meta">
    <span class="name">{file.name}</span>
    <span class="row">
      <span class={badgeClass(file.status)}>{t(`status.${file.status}`)}</span>
      <span class="num size">{formatBytes(file.size)}</span>
    </span>
  </span>
</button>

<style>
  .tile {
    display: flex;
    flex-direction: column;
    padding: 0;
    margin: 1px;
    overflow: hidden;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: var(--surface);
    cursor: pointer;
    text-align: left;
  }
  .tile.on { margin: 0; border: 2px solid var(--brand); box-shadow: 0 0 0 3px var(--brand-soft); }
  .thumb { position: relative; display: block; width: 100%; aspect-ratio: 4 / 3; }
  .thumb.partial { background-image: linear-gradient(to bottom, transparent 62%, #6e737c 62%); }
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
  .check {
    position: absolute;
    top: 8px;
    right: 8px;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--brand);
    border: 2px solid #fff;
    color: #fff;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .meta { display: flex; flex-direction: column; gap: 6px; padding: 10px 10px 12px; min-width: 0; }
  .name { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 6px; }
  .size { font-size: 11px; color: var(--text-3); white-space: nowrap; }
</style>
