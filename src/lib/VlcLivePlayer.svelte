<script>
  import { onMount, onDestroy, createEventDispatcher } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import LiveProgramme from "./LiveProgramme.svelte";
  import { unpackVlcFrame } from "./vlcFrames.js";

  export let src = "";
  export let title = "";
  export let channelLogo = "";
  export let channelSourceId = "";
  export let channelTvgId = "";
  export let channels = [];
  export let channelIndex = -1;
  export let live = true;

  const dispatch = createEventDispatcher();
  const session = crypto.randomUUID();
  let state = "opening";
  let error = "";
  let volume = 100;
  let muted = false;
  let fullscreen = false;
  let showControls = true;
  let started = false;
  let destroyed = false;
  let polling = false;
  let loading = false;
  let timer;
  let controlsTimer;
  let unlistenResize;
  let canvas;
  let scratch;
  let animation;
  let lastFrame = 0;
  let lastRequestAt = 0;
  let tuneStartedAt = 0;
  let hasFrame = false;
  $: playing = state === "playing";
  $: buffering = !hasFrame && !error && state !== "paused";

  function revealControls() {
    showControls = true;
    window.dispatchEvent(new CustomEvent("videoControlsVisibility", { detail: { visible: true } }));
    clearTimeout(controlsTimer);
    controlsTimer = setTimeout(() => {
      if (!destroyed && state === "playing" && !error) {
        showControls = false;
        window.dispatchEvent(new CustomEvent("videoControlsVisibility", { detail: { visible: false } }));
      }
    }, 2500);
  }

  async function tune() {
    if (loading || destroyed) return;
    loading = true;
    error = "";
    state = "opening";
    started = false;
    hasFrame = false;
    lastFrame = 0;
    tuneStartedAt = Date.now();
    canvas?.getContext("2d", { alpha: false })?.clearRect(0, 0, canvas.width, canvas.height);
    try {
      // The native mpv video surface closes before VLC's surface is shown.
      await invoke("mpv_run_command", { args: ["stop"] });
      if (destroyed) return;
      await invoke("vlc_load", { session, url: src, volume, muted });
      if (destroyed) { await invoke("vlc_stop", { session }); return; }
      started = true;
      await poll();
    } catch (cause) {
      if (!destroyed) { error = String(cause); state = "error"; revealControls(); }
    } finally { loading = false; }
  }

  async function poll() {
    if (!started || destroyed || polling) return;
    polling = true;
    try {
      const status = await invoke("vlc_status", { session });
      if (!destroyed && status) {
        const previous = state;
        state = status.state;
        if (state !== previous) console.info("[live-vlc] state", state, "video frames", status.video_frames);
        if (state === "error" || state === "ended") {
          error = state === "ended" ? "The channel stream ended. Reconnect to continue watching." : "The channel could not continue playing. Reconnect to try again.";
          revealControls();
        } else if (state === "playing" && previous !== "playing") {
          error = "";
          revealControls();
        }
        if (!hasFrame && Date.now() - tuneStartedAt > 30_000 && state !== "paused") {
          error = "No video frames have arrived from this channel. Reconnect to try again.";
          revealControls();
        }
      }
    } catch (cause) { if (!destroyed) error = String(cause); }
    finally { polling = false; }
  }

  function drawFrame() {
    if (!canvas || !scratch || !hasFrame) return;
    const width = Math.max(1, Math.round(canvas.clientWidth * window.devicePixelRatio));
    const height = Math.max(1, Math.round(canvas.clientHeight * window.devicePixelRatio));
    if (canvas.width !== width || canvas.height !== height) { canvas.width = width; canvas.height = height; }
    const context = canvas.getContext("2d", { alpha: false });
    const scale = Math.min(width / scratch.width, height / scratch.height);
    const w = scratch.width * scale, h = scratch.height * scale;
    context.fillStyle = "#000";
    context.fillRect(0, 0, width, height);
    context.drawImage(scratch, (width - w) / 2, (height - h) / 2, w, h);
  }

  async function renderFrame(timestamp) {
    if (destroyed) return;
    try {
      if (started && timestamp - lastRequestAt >= 30 && (state !== "paused" || !hasFrame)) {
        lastRequestAt = timestamp;
        const payload = await invoke("vlc_frame", { session, after: lastFrame });
        if (destroyed) return;
        const frame = unpackVlcFrame(payload);
        if (frame) {
          lastFrame = frame.serial;
          if (scratch.width !== frame.width || scratch.height !== frame.height) {
            scratch.width = frame.width; scratch.height = frame.height;
          }
          scratch.getContext("2d", { alpha: false }).putImageData(new ImageData(frame.pixels, frame.width, frame.height), 0, 0);
          if (!hasFrame) console.info("[live-vlc] first video frame", frame.width, frame.height);
          hasFrame = true;
          error = "";
          drawFrame();
        }
      }
    } catch (cause) { if (!destroyed) error = String(cause); }
    if (!destroyed) animation = requestAnimationFrame(renderFrame);
  }

  async function control(values) {
    if (!started) return;
    try { await invoke("vlc_control", { session, ...values }); }
    catch (cause) { if (!destroyed) error = String(cause); }
    revealControls();
  }

  function togglePlay() { control({ paused: state !== "paused" }); }
  function toggleMute() { muted = !muted; control({ muted }); }
  function changeVolume(value) { volume = Math.max(0, Math.min(100, Number(value))); control({ volume }); }

  async function toggleFullscreen() {
    const target = getCurrentWindow();
    fullscreen = !(await target.isFullscreen());
    await target.setFullscreen(fullscreen);
    revealControls();
  }

  function switchChannel(step) {
    if (channels.length < 2 || channelIndex < 0) return;
    const index = (channelIndex + step + channels.length) % channels.length;
    const channel = channels[index];
    window.dispatchEvent(new CustomEvent("openVideoPlayer", { detail: {
      src: channel.url, title: channel.name, channelLogo: channel.logo,
      channelSourceId: channel.source_id, channelTvgId: channel.tvg_id,
      live: true, channels, channelIndex: index,
    } }));
  }

  function handleKey(event) {
    if (["INPUT", "SELECT", "TEXTAREA"].includes(event.target?.tagName)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      if (fullscreen) toggleFullscreen(); else dispatch("close");
    } else if (event.code === "Space") { event.preventDefault(); togglePlay(); }
    else if (event.key === "PageUp") { event.preventDefault(); switchChannel(-1); }
    else if (event.key === "PageDown") { event.preventDefault(); switchChannel(1); }
    else if (event.key.toLowerCase() === "m") toggleMute();
    else if (event.key.toLowerCase() === "f") toggleFullscreen();
    else if (event.key === "ArrowUp") { event.preventDefault(); changeVolume(volume + 5); }
    else if (event.key === "ArrowDown") { event.preventDefault(); changeVolume(volume - 5); }
  }

  onMount(() => {
    scratch = document.createElement("canvas");
    animation = requestAnimationFrame(renderFrame);
    if (live) tune();
    timer = setInterval(poll, 500);
    window.addEventListener("keydown", handleKey);
    const target = getCurrentWindow();
    target.isFullscreen().then((value) => { if (!destroyed) fullscreen = value; }).catch(() => {});
    target.listen("tauri://resize", async () => { const value = await target.isFullscreen(); if (!destroyed) { fullscreen = value; drawFrame(); } })
      .then((stop) => { if (destroyed) stop(); else unlistenResize = stop; }).catch(() => {});
    revealControls();
  });

  onDestroy(() => {
    destroyed = true;
    clearInterval(timer);
    clearTimeout(controlsTimer);
    cancelAnimationFrame(animation);
    unlistenResize?.();
    window.removeEventListener("keydown", handleKey);
    // Session-scoped stop cannot stop the next channel during a rapid switch.
    invoke("vlc_stop", { session }).catch(() => {});
    window.dispatchEvent(new CustomEvent("videoControlsVisibility", { detail: { visible: true } }));
  });
</script>

<div class="video-player" class:fullscreen class:hide-cursor={!showControls && !error} on:mousemove={revealControls} role="region" aria-label="Live TV player">
  <canvas bind:this={canvas} class="video-canvas" aria-label="Live TV video"></canvas>
  <button class="video-surface" on:click={togglePlay} on:dblclick={toggleFullscreen} aria-label={playing ? "Pause live TV" : "Resume live TV"}></button>
  <div class="player-header" class:visible={showControls || !!error}>
    <button class="back-btn" on:click={() => dispatch("close")} aria-label="Back to Live TV"><i class="ri-arrow-left-line"></i></button>
    <div class="player-title">
      <span class="player-title-main">{title}</span>
      <span class="player-title-sub">Live TV{#if channels.length > 1 && channelIndex >= 0} · {channelIndex + 1} of {channels.length}{/if}</span>
      <LiveProgramme sourceId={channelSourceId} tvgId={channelTvgId} />
    </div>
  </div>
  {#if error}
    <div class="live-message" role="alert">
      <i class="ri-error-warning-line"></i><p>{error}</p>
      <button on:click={tune} disabled={loading}>Reconnect</button>
      <button on:click={() => dispatch("close")}>Back to channels</button>
    </div>
  {:else if buffering}
    <div class="live-message loading" role="status">
      {#if channelLogo}<img src={channelLogo} alt="" />{/if}
      <i class="ri-loader-4-line spin"></i><p>{started ? "Buffering…" : "Tuning in…"}</p>
    </div>
  {/if}
  <div class="live-controls" class:visible={showControls || state === "paused" || !!error}>
    <button on:click={togglePlay} aria-label={state === "paused" ? "Resume" : "Pause"}><i class={state === "paused" ? "ri-play-fill" : "ri-pause-fill"}></i></button>
    {#if channels.length > 1 && channelIndex >= 0}
      <button on:click={() => switchChannel(-1)} aria-label="Previous channel" title="Previous channel (Page Up)"><i class="ri-skip-back-fill"></i></button>
      <button on:click={() => switchChannel(1)} aria-label="Next channel" title="Next channel (Page Down)"><i class="ri-skip-forward-fill"></i></button>
    {/if}
    <span class="live-badge"><span class="live-dot"></span>LIVE</span>
    <div class="spacer"></div>
    <button on:click={toggleMute} aria-label={muted ? "Unmute" : "Mute"}><i class={muted || !volume ? "ri-volume-mute-fill" : "ri-volume-up-fill"}></i></button>
    <input type="range" min="0" max="100" step="1" value={volume} on:input={(event) => changeVolume(event.currentTarget.value)} aria-label="Volume" />
    <button on:click={toggleFullscreen} aria-label={fullscreen ? "Exit fullscreen" : "Enter fullscreen"}><i class={fullscreen ? "ri-fullscreen-exit-line" : "ri-fullscreen-line"}></i></button>
  </div>
</div>

<style>
  .video-canvas { position: absolute; inset: 0; width: 100%; height: 100%; background: #000; }
  .video-surface { position: absolute; inset: 0; width: 100%; height: 100%; background: transparent; border: 0; cursor: inherit; }
  .live-controls { display: flex; align-items: center; gap: 12px; position: absolute; bottom: 0; width: 100%; padding: 60px 28px 24px; box-sizing: border-box; background: linear-gradient(transparent, #000c); opacity: 0; pointer-events: none; transition: opacity .25s; }
  .live-controls.visible { opacity: 1; pointer-events: auto; }
  .live-controls button { background: none; color: #fff; border: 0; width: 40px; height: 40px; font-size: 23px; cursor: pointer; border-radius: 9px; }
  .live-controls button:hover { background: #ffffff20; }
  .spacer { flex: 1; }
  input[type="range"] { width: 100px; accent-color: var(--accent-color); }
  .live-message { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); max-width: min(540px, 80vw); padding: 24px; border-radius: 14px; background: #101014ed; text-align: center; color: var(--text-primary); }
  .live-message p { margin: 12px 0; line-height: 1.5; font-size: 14px; overflow-wrap: anywhere; }
  .live-message > i { font-size: 28px; }
  .live-message button { padding: 9px 14px; margin: 5px; background: #ffffff15; border: 1px solid #ffffff20; border-radius: 8px; color: inherit; font: inherit; font-size: 13px; cursor: pointer; }
  .live-message img { display: block; max-width: 100px; max-height: 65px; margin: 0 auto 16px; object-fit: contain; }
  .spin { display: inline-block; animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spin { animation: none; } .live-controls { transition: none; } }
</style>
