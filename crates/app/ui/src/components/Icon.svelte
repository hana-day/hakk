<script lang="ts" module>
  // Stroke icons from the mock screens, keyed by name.
  const PATHS = {
    file: '<path d="M4 4h10l6 6v10H4z"/><path d="M14 4v6h6"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 8v.5M12 11v5"/>',
    warn: '<path d="M12 3l10 18H2z"/><path d="M12 10v5M12 18v.5"/>',
    sd: '<path d="M7 2h8l4 4v16H7z"/><path d="M10 5v3M13 5v3M16 7v1"/>',
    hdd: '<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M7 14h4"/><circle cx="17" cy="14" r="1"/>',
    internal: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
    pause: '<path d="M8 5v14M16 5v14"/>',
    play: '<path d="M7 4l13 8-13 8z"/>',
    folder: '<path d="M3 6h7l2 2h9v11H3z"/>',
    photo: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M3 16l5-5 4 4 3-3 6 6"/>',
    video: '<rect x="3" y="6" width="13" height="12" rx="2"/><path d="M16 10l5-3v10l-5-3"/>',
    check: '<path d="M5 12.5l4.5 4.5L19 7"/>',
    up: '<path d="M12 19V5M5 12l7-7 7 7"/>',
    chevron: '<path d="M9 6l6 6-6 6"/>',
    download: '<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>',
    repair:
      '<path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L4 17l3 3 5.3-5.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.5-2.5z"/>',
  } as const;

  export type IconName = keyof typeof PATHS;
</script>

<script lang="ts">
  let {
    name,
    size = 16,
    stroke = 2,
  }: { name: IconName; size?: number; stroke?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={stroke}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  style="flex: none"
>
  {@html PATHS[name]}
</svg>
