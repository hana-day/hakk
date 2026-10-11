<script lang="ts">
  import type { RecoveryProgress } from '../../lib/bindings/RecoveryProgress';
  import { formatBytes } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Icon from '../Icon.svelte';
  import Msg from '../Msg.svelte';

  let { recovery, scanPaused }: { recovery: RecoveryProgress; scanPaused: boolean } = $props();

  const percent = $derived(
    recovery.bytesTotal > 0 ? Math.round((recovery.bytesDone / recovery.bytesTotal) * 100) : 0,
  );
</script>

<div role="status" class="banner">
  <span class="title">
    {#if scanPaused}<Icon name="pause" stroke={2.4} />{t('recoveryBanner.scanPaused')}
    {:else}<Icon name="download" stroke={2.4} />{t('recoveryBanner.recovering')}{/if}
  </span>
  <span>
    <Msg
      key="recoveryBanner.progress"
      params={{
        done: recovery.filesDone,
        total: recovery.filesTotal,
        bytes: t('recoveryBanner.bytes', {
          done: formatBytes(recovery.bytesDone),
          total: formatBytes(recovery.bytesTotal),
        }),
      }}
      mono={['bytes']}
    />
  </span>
  <span class="bar"><span style:width={`${percent}%`}></span></span>
  {#if scanPaused}<span>{t('recoveryBanner.resumeNote')}</span>{/if}
</div>

<style>
  .banner {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px 18px;
    padding: 12px 24px;
    background: var(--warn-soft);
    border-bottom: 1px solid var(--warn-line);
    color: var(--warn-strong);
    font-size: 14px;
  }
  .title { display: inline-flex; align-items: center; gap: 8px; font-weight: 700; }
  .bar { flex: 1 1 160px; max-width: 280px; height: 6px; border-radius: 3px; background: var(--warn-line); overflow: hidden; }
  .bar span { display: block; height: 100%; background: var(--warn-bar); transition: width 0.2s linear; }
</style>
