<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';
  import type { Destination } from '../lib/bindings/Destination';
  import { driveTitle, errorText } from '../lib/display';
  import { t } from '../lib/i18n';
  import {
    applyFilters,
    buildTree,
    formatLabel,
    inType,
    noFilters,
    passesOthers,
    sortFiles,
    typeCounts,
    yearsOf,
    type Filters,
    type SortOrder,
    type ViewMode,
  } from '../lib/results';
  import type { ScanSession } from '../lib/session.svelte';
  import { showToast } from '../lib/toast.svelte';
  import Dialog from '../components/Dialog.svelte';
  import Icon from '../components/Icon.svelte';
  import Logo from '../components/Logo.svelte';
  import Msg from '../components/Msg.svelte';
  import DestinationDialog from '../components/results/DestinationDialog.svelte';
  import PreviewPane from '../components/results/PreviewPane.svelte';
  import ProgressPanel from '../components/results/ProgressPanel.svelte';
  import RecoveryBanner from '../components/results/RecoveryBanner.svelte';
  import RecoveryDoneDialog from '../components/results/RecoveryDoneDialog.svelte';
  import SelectionBar from '../components/results/SelectionBar.svelte';
  import Sidebar from '../components/results/Sidebar.svelte';
  import SummaryPanel from '../components/results/SummaryPanel.svelte';
  import ThumbnailView from '../components/results/ThumbnailView.svelte';
  import Toolbar from '../components/results/Toolbar.svelte';
  import TreeTable from '../components/results/TreeTable.svelte';

  let { session, onleave }: { session: ScanSession; onleave: () => Promise<void> } = $props();

  let filters = $state<Filters>({ ...noFilters });
  let mode = $state<ViewMode>('list');
  let sort = $state<SortOrder>('newest');
  /** Open folders of the list. */
  const open = new SvelteSet<string>();
  let previewId = $state<number | null>(null);

  let choosingDestination = $state(false);
  /** Files to recover once a destination is chosen (a single file from the preview). */
  let pendingRecovery = $state<number[] | null>(null);
  let confirming = $state<'cancel' | 'leave' | null>(null);

  /** Files passing every filter but the type, for the sidebar's counts. */
  const others = $derived(session.allFiles.filter((f) => passesOthers(f, filters)));
  const counts = $derived(typeCounts(others));
  const years = $derived(yearsOf(session.allFiles));
  const matching = $derived(applyFilters(session.shownFiles, filters));
  const sorted = $derived(sortFiles(matching, sort));
  const tree = $derived(buildTree(matching, sort));
  const pendingCount = $derived(
    session.pendingFiles.filter((f) => inType(f, filters.type) && passesOthers(f, filters)).length,
  );
  const hiddenCount = $derived(session.allFiles.filter((f) => f.hidden !== null).length);
  const previewed = $derived(previewId === null ? null : (session.file(previewId) ?? null));

  // Open the top folders (volumes, the carved files) when they first appear.
  $effect(() => {
    for (const root of tree) {
      if (!seenRoots.has(root.key)) {
        seenRoots.add(root.key);
        open.add(root.key);
      }
    }
  });
  const seenRoots = new Set<string>();

  const place = $derived.by(() => {
    const k = filters.type;
    if (k.kind === 'all') return t('sidebar.all');
    const kind = k.kind === 'photo' ? t('sidebar.photos') : t('sidebar.videos');
    return k.format === null ? kind : `${kind}${t('results.breadcrumbSeparator')}${formatLabel(k.format)}`;
  });

  const paused = $derived(session.progress?.state === 'paused');

  async function attempt(action: () => Promise<unknown>) {
    try {
      await action();
    } catch (err) {
      showToast(errorText(err), 'error');
    }
  }

  function toggle(id: number) {
    if (!session.selection.delete(id)) session.selection.add(id);
  }

  function selectMany(ids: number[], select: boolean) {
    for (const id of ids) {
      if (select) session.selection.add(id);
      else session.selection.delete(id);
    }
  }

  function recoverOne(id: number) {
    if (session.destination === null) {
      pendingRecovery = [id];
      choosingDestination = true;
      return;
    }
    void attempt(() => session.recoverFiles([id]));
  }

  function chooseDestination(d: Destination) {
    session.destination = d;
    choosingDestination = false;
    const files = pendingRecovery;
    pendingRecovery = null;
    if (files !== null) void attempt(() => session.recoverFiles(files));
  }

  // Report finished repairs.
  $effect(() => {
    const report = session.lastRepair;
    if (report === null) return;
    const name = session.file(report.file)?.name ?? '';
    showToast(
      report.succeeded ? t('repair.done', { name }) : t('repair.failed', { name }),
      report.succeeded ? 'info' : 'error',
    );
    session.lastRepair = null;
  });

  async function confirmed() {
    const what = confirming;
    confirming = null;
    if (what === 'cancel') await attempt(() => session.cancel());
    if (what === 'leave') await attempt(onleave);
  }
</script>

<div class="page">
  <header>
    <div class="left">
      <button type="button" class="home" aria-label={t('header.home')} onclick={() => (confirming = 'leave')}>
        <Logo />
      </button>
      <span class="divider"></span>
      <span class="drive">{driveTitle(session.drive)}</span>
    </div>
    <div class="actions">
      {#if session.scanning}
        {#if session.switching === 'pausing'}
          <button type="button" class="btn-secondary" disabled>{t('header.pausing')}</button>
        {:else if session.switching === 'resuming'}
          <button type="button" class="btn-secondary" disabled>{t('header.resuming')}</button>
        {:else if paused}
          <button type="button" class="btn-secondary" onclick={() => attempt(() => session.resume())}>
            <Icon name="play" size={14} />{t('header.resume')}
          </button>
        {:else}
          <button
            type="button"
            class="btn-secondary"
            disabled={session.progress?.state === 'pausedForRecovery'}
            onclick={() => attempt(() => session.pause())}
          >
            <Icon name="pause" size={14} stroke={2.4} />{t('header.pause')}
          </button>
        {/if}
        <button type="button" class="btn-quiet" onclick={() => (confirming = 'cancel')}>{t('header.cancel')}</button>
      {:else}
        <span class="saved-note">{t('header.saved')}</span>
      {/if}
    </div>
  </header>

  {#if session.summary}
    <SummaryPanel summary={session.summary} />
  {:else if session.progress}
    <ProgressPanel progress={session.progress} />
  {/if}

  {#if session.recovery}
    <RecoveryBanner
      recovery={session.recovery}
      scanPaused={session.progress?.state === 'pausedForRecovery'}
    />
  {/if}

  <div class="body">
    <Sidebar {counts} type={filters.type} onselect={(type) => (filters.type = type)} />

    <main>
      <Toolbar bind:filters bind:mode bind:sort {years} />

      <div class="context">
        <span><Msg key="results.context" params={{ place, count: matching.length }} mono={['count']} /></span>
      </div>

      {#if pendingCount > 0}
        <button type="button" class="show-new" onclick={() => session.showPending()}>
          <Icon name="up" size={14} stroke={2.4} />
          <Msg key="results.showNew" params={{ count: pendingCount }} />
        </button>
      {/if}

      {#if matching.length === 0}
        <p class="empty">
          {session.scanning && session.allFiles.length === 0 ? t('results.emptyScanning') : t('results.empty')}
        </p>
      {:else if mode === 'list'}
        <TreeTable
          scanId={session.scanId}
          roots={tree}
          {open}
          selection={session.selection}
          {previewId}
          ontoggle={toggle}
          onselectmany={selectMany}
          onpreview={(id) => (previewId = id)}
        />
      {:else}
        <ThumbnailView
          scanId={session.scanId}
          files={sorted}
          selection={session.selection}
          {previewId}
          ontoggle={toggle}
          onpreview={(id) => (previewId = id)}
        />
      {/if}

      {#if hiddenCount > 0}
        <p class="hidden-note">
          {filters.showHidden ? t('results.hiddenShown') : t('results.hiddenNote', { count: hiddenCount })}
          <button type="button" class="btn-link" onclick={() => (filters.showHidden = !filters.showHidden)}>
            {filters.showHidden ? t('results.hideHidden') : t('results.showHidden')}
          </button>
        </p>
      {/if}
    </main>

    <PreviewPane
      scanId={session.scanId}
      file={previewed}
      selected={previewId !== null && session.selection.has(previewId)}
      ontoggle={toggle}
      onrecover={recoverOne}
    />
  </div>

  <SelectionBar
    count={session.selection.size}
    size={session.selectedSize}
    destination={session.destination}
    recovering={session.recovery !== null}
    onchange={() => (choosingDestination = true)}
    onrecover={() => attempt(() => session.recover())}
  />
</div>

<DestinationDialog
  open={choosingDestination}
  destinations={session.destinations}
  current={session.destination}
  onchoose={chooseDestination}
  onclose={() => {
    choosingDestination = false;
    pendingRecovery = null;
  }}
/>

<RecoveryDoneDialog report={session.recoveryReport} onclose={() => (session.recoveryReport = null)} />

<Dialog
  open={confirming !== null}
  title={confirming === 'cancel' ? t('confirm.cancelScan.title') : t('confirm.leave.title')}
  onclose={() => (confirming = null)}
>
  <p>
    {#if confirming === 'cancel'}{t('confirm.cancelScan.body')}
    {:else if session.scanning}{t('confirm.leave.bodyScanning')}
    {:else}{t('confirm.leave.bodyDone')}{/if}
  </p>
  {#snippet actions()}
    <button type="button" class="btn-quiet" onclick={() => (confirming = null)}>
      {confirming === 'cancel' ? t('confirm.cancelScan.keep') : t('common.cancel')}
    </button>
    <button type="button" class="btn-primary" onclick={confirmed}>
      {confirming === 'cancel' ? t('confirm.cancelScan.confirm') : t('confirm.leave.confirm')}
    </button>
  {/snippet}
</Dialog>

<style>
  .saved-note { font-size: 13px; color: var(--text-3); }
  /* Header, progress and footer stay put; only the sidebar and the results scroll. */
  .page { height: 100vh; display: flex; flex-direction: column; }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    padding: 12px 24px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }
  .left { display: flex; align-items: center; flex-wrap: wrap; gap: 10px 16px; }
  .home {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    cursor: pointer;
  }
  .divider { width: 1px; height: 20px; background: var(--line); }
  .drive { font-size: 14px; color: var(--text-2); }
  .actions { display: flex; align-items: center; gap: 8px; }
  .body { flex: 1 1 auto; min-height: 0; display: flex; align-items: stretch; }
  main {
    flex: 1 1 auto;
    min-width: 0;
    overflow-y: auto;
    padding: 18px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .context { font-size: 14px; color: var(--text-3); }
  .show-new {
    position: sticky;
    top: 12px;
    z-index: 4;
    align-self: center;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 0 18px;
    border-radius: 20px;
    border: 0;
    background: var(--text);
    color: #fff;
    font-size: 14px;
    cursor: pointer;
    box-shadow: 0 4px 12px rgba(21, 24, 29, 0.2);
  }
  .empty { margin: 40px 0; text-align: center; color: var(--text-3); }
  .hidden-note { margin: 0; font-size: 13px; color: var(--text-3); text-align: center; }
  @media (max-width: 860px) {
    .body { flex-direction: column; }
    .body :global(nav) { flex: none; max-height: 30vh; border-right: 0; border-bottom: 1px solid var(--line); }
  }
</style>
