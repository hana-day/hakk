<script lang="ts">
  // Scan progress on one line: what is happening, the disk map as a band, and the numbers.
  import type { ScanProgress } from '../../lib/bindings/ScanProgress';
  import { formatBytes, formatCount } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Msg from '../Msg.svelte';
  import DiskMap from './DiskMap.svelte';

  let { progress }: { progress: ScanProgress } = $props();

  const percent = $derived(
    progress.totalBytes > 0 ? Math.floor((progress.bytesRead / progress.totalBytes) * 100) : 0,
  );
  const carving = $derived(progress.stage === 'carving');
  // Retrying unreadable areas can outlast the main pass on a failing disk, so it gets its
  // own wording instead of sitting at 100 % under "scanning".
  const retry = $derived(progress.stage === 'retrying' ? progress.retry : null);
  const paused = $derived(progress.state === 'paused');
  const minutes = $derived(
    progress.remainingSeconds === null ? null : Math.round(progress.remainingSeconds / 60),
  );
  const title = $derived(
    paused
      ? t('progress.paused')
      : retry
        ? t('progress.retrying')
        : carving
          ? t('progress.carving')
          : t('progress.fileSystem'),
  );
  const step = $derived(
    retry
      ? t('progress.retryPass', { pass: retry.pass, passes: retry.passes })
      : t('progress.stageOf', { n: carving ? 2 : 1, total: 2 }),
  );
</script>

<section aria-label={t('progress.label')} class="panel">
  <span class="title">{title}</span>
  <span class="step">{step}</span>
  <DiskMap map={progress.diskMap} {percent} errors={progress.readErrors} strip />
  <span class="stats">
    {#if retry}
      <span><Msg key="progress.retryLeft" params={{ bytes: formatBytes(retry.bytesLeft) }} /></span>
    {:else}
      <span class="num">
        {t('progress.bytes', { read: formatBytes(progress.bytesRead), total: formatBytes(progress.totalBytes) })}
      </span>
    {/if}
    {#if !paused && !retry}
      <span>
        {#if minutes === null}{t('progress.remainingUnknown')}
        {:else if minutes < 1}{t('progress.remainingUnderMinute')}
        {:else}<Msg key="progress.remaining" params={{ minutes }} />{/if}
      </span>
      {#if progress.bytesPerSecond > 0}
        <span class="num">{t('progress.rate', { rate: formatBytes(progress.bytesPerSecond, 0) })}</span>
      {/if}
    {/if}
    {#if progress.readErrors > 0}
      <span class="errors">{t('progress.readErrors', { count: formatCount(progress.readErrors) })}</span>
    {/if}
  </span>
  <span class="num percent">{percent}%</span>
</section>

<style>
  .panel {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px 16px;
    padding: 10px 24px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    font-size: 13px;
  }
  .title { font-size: 14px; font-weight: 700; white-space: nowrap; }
  .step { color: var(--text-3); white-space: nowrap; }
  .stats { display: inline-flex; align-items: baseline; flex-wrap: wrap; gap: 4px 14px; color: var(--text-2); }
  .errors { color: var(--danger, #b2402b); }
  .percent { font-size: 16px; font-weight: 700; color: var(--text); }
</style>
