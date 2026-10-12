<script lang="ts">
  // The chosen file: a large preview and what is known about it.
  import type { FoundFile } from '../../lib/bindings/FoundFile';
  import type { ScanId } from '../../lib/bindings/ScanId';
  import { displayName, statusBadge } from '../../lib/display';
  import { formatBytes, formatCount, formatDateTime } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import { folderPath, formatLabel } from '../../lib/results';
  import Thumb from './Thumb.svelte';

  let {
    scanId,
    file,
    selected,
    ontoggle,
    onrecover,
  }: {
    scanId: ScanId;
    file: FoundFile | null;
    selected: boolean;
    ontoggle: (id: number) => void;
    onrecover: (id: number) => void;
  } = $props();

  const place = $derived.by(() => {
    if (file === null) return '';
    const path = folderPath(file);
    return path === null ? t('results.carvedFolder') : path.join(t('results.folderSeparator'));
  });
</script>

<aside aria-label={t('preview.label')}>
  {#if file === null}
    <p class="empty">{t('preview.empty')}</p>
  {:else}
    {#key file.id}
      <div class="picture"><Thumb {scanId} {file} size={1024} fit="contain" /></div>
    {/key}
    <h2 title={displayName(file)}>{displayName(file)}</h2>
    {@const badge = statusBadge(file)}
    <dl>
      <dt>{t('preview.state')}</dt>
      <dd>
        <span class={badge.className}>{badge.text}</span>
        {file.deleted ? t('preview.deleted') : t('preview.existing')}
      </dd>
      <dt>{t('preview.location')}</dt>
      <dd class="path">{place}</dd>
      <dt>{t('preview.size')}</dt>
      <dd>{formatBytes(file.size)}</dd>
      <dt>{t('preview.date')}</dt>
      <dd>
        {#if file.date === null}{t('list.unknownDate')}
        {:else if file.date.source === 'fileSystem'}{t('list.fileDate', { date: formatDateTime(file.date.local) })}
        {:else}{formatDateTime(file.date.local)}{/if}
      </dd>
      <dt>{t('preview.format')}</dt>
      <dd>{formatLabel(file.format)}</dd>
      {#if file.width !== null && file.height !== null}
        <dt>{t('preview.dimensions')}</dt>
        <dd>{t('preview.dimensionsValue', { width: formatCount(file.width), height: formatCount(file.height) })}</dd>
      {/if}
      {#if file.offset !== null}
        <dt>{t('preview.offset')}</dt>
        <dd class="num">{formatCount(file.offset)}</dd>
      {/if}
    </dl>
    <div class="actions">
      <button type="button" class="btn-primary" onclick={() => file && onrecover(file.id)}>{t('preview.recover')}</button>
      <button type="button" class="btn-secondary" onclick={() => file && ontoggle(file.id)}>
        {selected ? t('preview.unselect') : t('preview.select')}
      </button>
    </div>
  {/if}
</aside>

<style>
  aside {
    flex: 0 0 300px;
    padding: 16px;
    background: var(--surface);
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    font-size: 13px;
  }
  .empty { margin: auto; color: var(--text-3); text-align: center; font-size: 14px; }
  .picture { width: 100%; aspect-ratio: 4 / 3; border-radius: 10px; overflow: hidden; background: var(--neutral-soft); }
  h2 { margin: 0; font-size: 15px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  dl { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 6px 12px; }
  dt { color: var(--text-3); }
  dd { margin: 0; min-width: 0; display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .path { word-break: break-all; }
  .badge { padding: 2px 8px; font-size: 11px; }
  .actions { display: flex; flex-direction: column; gap: 8px; }
  @media (max-width: 1100px) {
    aside { display: none; }
  }
</style>
