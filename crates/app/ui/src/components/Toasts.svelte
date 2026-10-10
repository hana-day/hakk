<script lang="ts">
  import { toasts } from '../lib/toast.svelte';
</script>

<div class="toasts" role="status" aria-live="polite">
  {#each toasts as toast (toast.id)}
    <div class="toast" class:error={toast.tone === 'error'}>{toast.text}</div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    left: 50%;
    bottom: 96px;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 20;
    pointer-events: none;
  }
  .toast {
    padding: 12px 18px;
    border-radius: 10px;
    background: var(--text);
    color: #fff;
    font-size: 14px;
    box-shadow: 0 6px 20px rgba(21, 24, 29, 0.2);
  }
  .error { background: var(--error); }
</style>
