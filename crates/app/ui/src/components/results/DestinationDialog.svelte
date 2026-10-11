<script lang="ts">
  import type { Destination } from '../../lib/bindings/Destination';
  import { formatBytes } from '../../lib/format';
  import { t } from '../../lib/i18n';
  import Dialog from '../Dialog.svelte';
  import Msg from '../Msg.svelte';

  let {
    open,
    destinations,
    current,
    onchoose,
    onclose,
  }: {
    open: boolean;
    destinations: Destination[];
    current: Destination | null;
    onchoose: (d: Destination) => void;
    onclose: () => void;
  } = $props();

  let pickedId = $state<string | null>(null);

  $effect(() => {
    if (open) pickedId = current?.id ?? null;
  });

  const picked = $derived(destinations.find((d) => d.id === pickedId && d.blocked === null));
</script>

<Dialog {open} title={t('destination.title')} {onclose}>
  <p class="note">{t('destination.note')}</p>
  <div class="list" role="radiogroup" aria-label={t('destination.title')}>
    {#each destinations as d (d.id)}
      <label class="option" class:blocked={d.blocked !== null} class:on={d.id === pickedId}>
        <input type="radio" name="destination" value={d.id} bind:group={pickedId} disabled={d.blocked !== null} />
        <span class="text">
          <span class="name">{t('place', { kind: t(`driveKind.${d.kind}`), name: d.name })}</span>
          {#if d.blocked !== null}
            <span class="reason">{t(`destination.blocked.${d.blocked}`)}</span>
          {:else}
            <span class="sub"><Msg key="destination.detail" params={{ path: d.path, size: formatBytes(d.freeBytes, 0) }} mono={['size']} /></span>
          {/if}
        </span>
      </label>
    {/each}
  </div>
  {#snippet actions()}
    <button type="button" class="btn-quiet" onclick={onclose}>{t('common.cancel')}</button>
    <button type="button" class="btn-primary" disabled={!picked} onclick={() => picked && onchoose(picked)}>
      {t('destination.choose')}
    </button>
  {/snippet}
</Dialog>

<style>
  .note { margin: 0 0 12px; }
  .list { display: flex; flex-direction: column; gap: 8px; }
  .option {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 10px;
    border: 1px solid var(--line);
    cursor: pointer;
  }
  .option.on { border-color: var(--brand); background: var(--brand-row); }
  .option.blocked { cursor: not-allowed; background: var(--bg); }
  input { margin-top: 4px; width: 18px; height: 18px; accent-color: var(--brand); }
  .text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .name { font-weight: 700; color: var(--text); }
  .blocked .name { color: var(--text-3); }
  .sub { font-size: 12px; color: var(--text-3); overflow-wrap: anywhere; }
  .reason { font-size: 12px; color: var(--warn); }
</style>
