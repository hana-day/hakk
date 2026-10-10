<script lang="ts">
  // Modal dialog built on <dialog>, which handles focus trapping and Escape.
  import type { Snippet } from 'svelte';

  let {
    open,
    title,
    onclose,
    children,
    actions,
  }: {
    open: boolean;
    title: string;
    onclose: () => void;
    children: Snippet;
    actions: Snippet;
  } = $props();

  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  });
</script>

<dialog bind:this={dialog} aria-labelledby="dialog-title" onclose={onclose}>
  <h2 id="dialog-title">{title}</h2>
  <div class="body">{@render children()}</div>
  <div class="actions">{@render actions()}</div>
</dialog>

<style>
  dialog {
    width: min(480px, calc(100vw - 32px));
    padding: 24px;
    border: 0;
    border-radius: 14px;
    box-shadow: 0 20px 50px rgba(21, 24, 29, 0.25);
    color: var(--text);
  }
  dialog::backdrop { background: rgba(21, 24, 29, 0.4); }
  h2 { margin: 0 0 12px; font-size: 18px; }
  .body { font-size: 14px; color: var(--text-2); line-height: 1.6; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 20px; }
</style>
