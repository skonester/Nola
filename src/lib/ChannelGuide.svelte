<script>
  import { onMount, createEventDispatcher } from "svelte";
  import { programmeTime, programmeProgress } from "./liveTvGuide.js";

  export let channel;
  export let programmes = [];
  export let now = Date.now() / 1000;
  export let loading = false;
  export let error = "";
  export let hasFeed = false;

  const dispatch = createEventDispatcher();
  let dialog;
  $: upcoming = programmes.filter((p) => p.stop > now);
  const day = (timestamp) => new Date(timestamp * 1000)
    .toLocaleDateString([], { weekday: "long", month: "short", day: "numeric" });

  onMount(() => dialog.showModal());
</script>

<dialog bind:this={dialog} on:close={() => dispatch("close")} aria-labelledby="channel-guide-title">
  <header>
    <div>
      <span class="eyebrow">PROGRAM GUIDE</span>
      <h2 id="channel-guide-title">{channel.name}</h2>
      <p>Times shown in your local timezone</p>
    </div>
    <button class="close" on:click={() => dialog.close()} aria-label="Close program guide"><i class="ri-close-line"></i></button>
  </header>
  <button class="watch" on:click={() => dispatch("play")}><i class="ri-play-fill"></i> Watch live</button>
  {#if error}<p class="warning" role="status">{error} Saved listings are shown when available.</p>{/if}
  <div class="schedule">
    {#each upcoming as programme, index}
      {#if index === 0 || day(programme.start) !== day(upcoming[index - 1].start)}
        <h3>{day(programme.start)}</h3>
      {/if}
      {@const current = programme.start <= now && programme.stop > now}
      <article class:current>
        <div class="time">{programmeTime(programme.start)}<span>{programmeTime(programme.stop)}</span></div>
        <div class="details">
          {#if current}<span class="on-now">ON NOW</span>{/if}
          <h4>{programme.title}</h4>
          {#if programme.subtitle}<p class="subtitle">{programme.subtitle}</p>{/if}
          {#if programme.description}<p class="description">{programme.description}</p>{/if}
          {#if current}<progress max="100" value={programmeProgress(programme, now)} aria-label="Current program progress"></progress>{/if}
        </div>
      </article>
    {:else}
      <p class="empty">{loading ? "Loading program listings…" : hasFeed ? "No upcoming listings are available for this channel." : "No program guide feed is configured for this source. Add an XMLTV feed in Sources."}</p>
    {/each}
  </div>
</dialog>

<style>
  dialog { width: min(680px, calc(100vw - 64px)); max-height: 80vh; padding: 24px; box-sizing: border-box; border: 1px solid rgba(255,255,255,.13); border-radius: 18px; background: #171719; color: var(--text-primary, #fff); box-shadow: 0 24px 100px #0009; }
  dialog::backdrop { background: #000b; backdrop-filter: blur(8px); }
  header { display: flex; justify-content: space-between; gap: 16px; }
  .eyebrow { font-size: 10px; letter-spacing: .12em; color: var(--text-tertiary); }
  h2 { margin: 6px 0; font-size: 23px; }
  header p { margin: 0; font-size: 12px; color: var(--text-tertiary); }
  button { font: inherit; color: inherit; cursor: pointer; border: 1px solid #ffffff20; border-radius: 8px; background: #ffffff0d; }
  .close { width: 32px; height: 32px; font-size: 20px; flex-shrink: 0; }
  .watch { margin-top: 20px; padding: 9px 14px; background: color-mix(in srgb, var(--accent-color) 25%, transparent); }
  .schedule { margin-top: 20px; }
  h3 { color: var(--text-secondary); font-size: 12px; font-weight: 500; margin: 20px 0 10px; }
  article { display: flex; gap: 18px; padding: 16px 12px; border-top: 1px solid #ffffff0d; }
  article.current { background: #ffffff05; border-radius: 10px; border-top-color: transparent; }
  .time { flex: 0 0 76px; font-size: 12px; line-height: 1.6; font-variant-numeric: tabular-nums; }
  .time span { display: block; color: var(--text-tertiary); }
  .details { min-width: 0; flex: 1; }
  h4 { margin: 0; font-size: 15px; font-weight: 600; }
  .on-now { color: var(--accent-color); display: block; font-size: 9px; letter-spacing: .1em; margin-bottom: 5px; }
  .subtitle { margin: 5px 0 0; font-size: 13px; color: var(--text-secondary); }
  .description { margin: 8px 0 0; font-size: 12px; color: var(--text-tertiary); line-height: 1.6; white-space: pre-line; }
  progress { display: block; width: 100%; height: 3px; margin-top: 14px; accent-color: var(--accent-color); }
  .empty, .warning { font-size: 13px; color: var(--text-secondary); line-height: 1.6; }
  .warning { color: #fbbf24; }
  .empty { padding: 30px 0; }
</style>
