<script lang="ts">
  import { api } from '../lib/api';
  import type { Drive } from '../lib/bindings/Drive';
  import { driveIcon, errorText } from '../lib/display';
  import { formatBytes } from '../lib/format';
  import { t } from '../lib/i18n';
  import { showToast } from '../lib/toast.svelte';
  import Icon from '../components/Icon.svelte';
  import Logo from '../components/Logo.svelte';

  let { onstart }: { onstart: (drive: Drive) => Promise<void> } = $props();

  let drives = $state<Drive[] | null>(null);
  let failed = $state(false);
  let pickedId = $state<string | null>(null);
  let starting = $state(false);

  const picked = $derived(drives?.find((d) => d.id === pickedId) ?? null);

  async function load() {
    drives = null;
    failed = false;
    try {
      drives = await api.listDrives();
      // Preselect the first drive that is likely to work (not an SSD with TRIM).
      pickedId = (drives.find((d) => d.warnings.length === 0) ?? drives[0])?.id ?? null;
    } catch {
      failed = true;
    }
  }

  $effect(() => {
    void load();
  });

  async function start() {
    if (!picked) return;
    starting = true;
    try {
      await onstart(picked);
    } catch (err) {
      showToast(errorText(err), 'error');
      starting = false;
    }
  }

  // Arrow keys move the choice, as in a native radio group.
  function onkeydown(e: KeyboardEvent) {
    if (!drives || drives.length === 0) return;
    const step = e.key === 'ArrowDown' || e.key === 'ArrowRight' ? 1 : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? -1 : 0;
    if (step === 0) return;
    e.preventDefault();
    const i = drives.findIndex((d) => d.id === pickedId);
    const next = drives[(i + step + drives.length) % drives.length];
    if (next) {
      pickedId = next.id;
      document.getElementById(`drive-${next.id}`)?.focus();
    }
  }
</script>

<div class="page">
  <header>
    <div class="brand"><Logo /></div>
    <button type="button" class="link" onclick={() => showToast(t('common.notAvailable'))}>
      <Icon name="file" />
      {t('driveSelect.openImage')}
    </button>
  </header>

  <main>
    <h1>{t('driveSelect.title')}</h1>

    {#if drives === null && !failed}
      <p class="note">{t('driveSelect.loading')}</p>
    {:else if failed || drives?.length === 0}
      <div class="note">
        <p>{failed ? t('driveSelect.loadFailed') : t('driveSelect.empty')}</p>
        <button type="button" class="btn-secondary" onclick={load}>{t('common.reload')}</button>
      </div>
    {:else if drives}
      <!-- svelte-ignore a11y_interactive_supports_focus -->
      <div role="radiogroup" aria-label={t('driveSelect.listLabel')} class="drives" tabindex="-1" {onkeydown}>
        {#each drives as d (d.id)}
          {@const on = d.id === pickedId}
          <button
            id={`drive-${d.id}`}
            type="button"
            role="radio"
            aria-checked={on}
            tabindex={on ? 0 : -1}
            class="card"
            class:on
            onclick={() => (pickedId = d.id)}
          >
            <span class="icon"><Icon name={driveIcon(d.kind)} size={26} stroke={1.8} /></span>
            <span class="text">
              <span class="title">
                <span class="name">{d.name}</span>
                <span class="sub">{t(`driveKind.${d.kind}`)}</span>
              </span>
              {#if d.fileSystem}<span class="sub">{d.fileSystem}</span>{/if}
              {#each d.warnings as w (w)}
                <span class="warn"><Icon name="warn" size={14} stroke={2.2} />{t(`driveWarning.${w}`)}</span>
              {/each}
            </span>
            <span class="side">
              <span class="num size">{formatBytes(d.capacityBytes)}</span>
              <span class="dot" class:on></span>
            </span>
          </button>
        {/each}
      </div>

      <div class="bottom">
        <span class="hint"><Icon name="info" />{t('driveSelect.doNotSave')}</span>
        <button type="button" class="btn-primary" disabled={!picked || starting} onclick={start}>
          <Icon name="search" size={18} stroke={2.2} />
          {t('driveSelect.start')}
        </button>
      </div>
    {/if}
  </main>
</div>

<style>
  .page { min-height: 100vh; display: flex; flex-direction: column; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    padding: 14px 24px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }
  .brand { display: flex; align-items: center; gap: 10px; }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 44px;
    border: 0;
    background: none;
    font-size: 14px;
    color: var(--brand);
    cursor: pointer;
  }
  .link:hover { color: var(--brand-hover); }
  main {
    width: 100%;
    max-width: 880px;
    margin: 0 auto;
    padding: 56px 24px 48px;
    display: flex;
    flex-direction: column;
    gap: 28px;
  }
  h1 { margin: 0; font-size: 30px; line-height: 1.35; font-weight: 700; }
  .note { color: var(--text-3); font-size: 15px; display: flex; flex-direction: column; align-items: flex-start; gap: 12px; }
  .note p { margin: 0; }
  .drives { display: flex; flex-direction: column; gap: 12px; outline: none; }
  .card {
    display: flex;
    align-items: center;
    gap: 16px;
    width: 100%;
    min-height: 88px;
    padding: 16px 20px;
    margin: 1px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: var(--surface);
    cursor: pointer;
    text-align: left;
  }
  .card.on { margin: 0; border: 2px solid var(--brand); box-shadow: 0 0 0 4px var(--brand-soft); }
  .icon {
    flex: none;
    width: 52px;
    height: 52px;
    border-radius: 12px;
    background: var(--neutral-soft);
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-2);
  }
  .text { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .title { display: flex; align-items: baseline; flex-wrap: wrap; gap: 4px 10px; }
  .name { font-size: 17px; font-weight: 700; }
  .sub { font-size: 13px; color: var(--text-3); }
  .warn { display: inline-flex; align-items: center; gap: 6px; margin-top: 4px; font-size: 13px; color: var(--warn); }
  .side { flex: none; display: flex; flex-direction: column; align-items: flex-end; gap: 8px; }
  .size { font-size: 14px; }
  .dot { width: 20px; height: 20px; border-radius: 50%; border: 2px solid #a9b0bc; background: var(--surface); }
  .dot.on { border: 6px solid var(--brand); }
  .bottom { display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: 16px; }
  .hint { display: inline-flex; align-items: center; gap: 8px; font-size: 14px; color: var(--text-3); }
</style>
