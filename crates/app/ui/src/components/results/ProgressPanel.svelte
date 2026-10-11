<script lang="ts">
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
</script>

<section aria-label={t('progress.label')} class="panel">
  <div class="top">
    <div class="stage">
      <span class="pre">
        {#if retry}
          {t('progress.stageAfter', {
            done: t('progress.carvingDone'),
            stage: t('progress.retryPass', { pass: retry.pass, passes: retry.passes }),
          })}
        {:else if carving}
          {t('progress.stageAfter', {
            done: t('progress.fileSystemDone'),
            stage: t('progress.stageOf', { n: 2, total: 2 }),
          })}
        {:else}
          {t('progress.stageOf', { n: 1, total: 2 })}
        {/if}
      </span>
      <span class="title">
        {paused
          ? t('progress.paused')
          : retry
            ? t('progress.retrying')
            : carving
              ? t('progress.carving')
              : t('progress.fileSystem')}
      </span>
    </div>
    <div class="stats">
      {#if retry}
        <span><Msg key="progress.retryLeft" params={{ bytes: formatBytes(retry.bytesLeft) }} /></span>
      {:else}
        <span class="num">
          {t('progress.bytes', {
            read: formatBytes(progress.bytesRead),
            total: formatBytes(progress.totalBytes),
          })}
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
      <span class="num percent">{percent}%</span>
    </div>
  </div>
  <div
    class="bar"
    role="progressbar"
    aria-valuenow={percent}
    aria-valuemin={0}
    aria-valuemax={100}
    aria-label={t('progress.label')}
  >
    <div style:width={`${percent}%`}></div>
  </div>
  <div class="bottom">
    <DiskMap map={progress.diskMap} {percent} errors={progress.readErrors} />
    <div class="counts">
      <div class="stat">
        <span class="stat-label">{t('progress.photos')}</span>
        <span class="num count">{formatCount(progress.photosFound)}</span>
      </div>
      <div class="stat">
        <span class="stat-label">{t('progress.videos')}</span>
        <span class="num count">{formatCount(progress.videosFound)}</span>
      </div>
    </div>
  </div>
</section>

<style>
  .panel {
    background: var(--surface);
    border-bottom: 1px solid var(--line);
    padding: 18px 24px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .top { display: flex; justify-content: space-between; align-items: flex-end; flex-wrap: wrap; gap: 12px; }
  .stage { display: flex; flex-direction: column; gap: 4px; }
  .pre { font-size: 13px; color: var(--text-3); }
  .title { font-size: 18px; font-weight: 700; }
  .stats { display: flex; align-items: baseline; flex-wrap: wrap; gap: 6px 20px; font-size: 14px; color: var(--text-2); }
  .percent { font-size: 28px; color: var(--text); }
  .bar { height: 8px; border-radius: 4px; background: var(--track); overflow: hidden; }
  .bar div { height: 100%; background: var(--brand); transition: width 0.2s linear; }
  .bottom { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 20px; align-items: flex-start; }
  .counts { flex: 0 1 260px; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
  .counts .stat { padding: 12px 14px; }
  .count { font-size: 22px; }
</style>
