<script>
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { nowAndNext, programmeTime } from "./liveTvGuide.js";

  export let sourceId = "";
  export let tvgId = "";
  let programmes = [];
  let now = Date.now() / 1000;
  let clock;
  let unlisten;
  let destroyed = false;
  $: listings = nowAndNext(programmes, now);

  async function reload() {
    if (!sourceId || !tvgId) return;
    const data = await invoke("get_live_programmes", { sourceId, tvgId });
    if (!destroyed) programmes = data;
  }

  onMount(() => {
    reload().catch(() => {});
    clock = setInterval(() => (now = Date.now() / 1000), 30_000);
    listen("live-tv-updated", () => reload().catch(() => {}))
      .then((stop) => { if (destroyed) stop(); else unlisten = stop; }).catch(() => {});
  });
  onDestroy(() => { destroyed = true; clearInterval(clock); unlisten?.(); });
</script>

{#if listings.current || listings.next}
  <span class="live-programmes">
    {#if listings.current}<span><strong>Now</strong> {listings.current.title}</span>{/if}
    {#if listings.next}<span class="next">Next · {programmeTime(listings.next.start)} · {listings.next.title}</span>{/if}
  </span>
{/if}

<style>
  .live-programmes { display: flex; flex-direction: column; gap: 4px; font-size: 13px; margin-top: 7px; max-width: min(65vw, 700px); }
  .live-programmes > span { overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
  strong { color: var(--accent-color); margin-right: 5px; font-size: 11px; font-weight: 600; }
  .next { color: #ffffff80; font-size: 11px; }
</style>
