<script lang="ts">
  // Renders a message with placeholders in the numeric (monospace) style.
  // `mono` limits the styling to the named placeholders; by default all are styled.
  import { parts, type MessageKey, type Params } from '../lib/i18n';

  let {
    key,
    params = {},
    mono,
  }: { key: MessageKey; params?: Params; mono?: string[] } = $props();
</script>

{#each parts(key, params) as part, i (i)}{#if part.param !== null && (mono === undefined || mono.includes(part.param))}<span
      class="num">{part.text}</span
    >{:else}{part.text}{/if}{/each}
