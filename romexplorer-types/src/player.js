// The audio player both UIs use (the web console loads it from /player.js, the desktop app
// evaluates the same text). One `<audio>` element, a thin bar that goes at the top of a
// title / release page, and the playlist as a panel down the right edge of the window.
//
// rxPlayer.start(barHost, tracks, labels, options)
//   barHost: the element the bar is drawn into (the page puts it at its very top)
//   tracks:  [{ id, url, title, artist, album, file_name, duration_ms, loudness_lufs }]
//   labels:  translated text for the buttons (the UI's own language layer supplies them)
//   options: { keep: true } -- the `audio_keep_playing` setting (the playlist says it)
//            hidden: [tracks] -- tracks hidden from this page, listed under the playlist
//            onHide(track, hidden) -- called by the "don't play this track" button (hidden
//              true) and by "restore" (false); the page records it. Without it, no button.
// rxPlayer.leave(): the page is being left. With `keep`, the bar moves to a strip along the
//   bottom of the window and the music goes on until another title page starts its own (or
//   stops it); without, this is stop().
// rxPlayer.stop(): stops playback and removes the bar and the playlist.
//
// Volume and "normalise" are remembered on this machine. Normalise scales each track by its
// measured loudness (`loudness_lufs`, from the audio scan) towards -16 LUFS, turning loud
// tracks down and never a quiet one up, so nothing clips; a track with no measurement plays
// as it is. It is the element's volume that is scaled -- no Web Audio graph, which would
// need the audio to be same-origin and would not survive a streamed track on a client.
// A ROM track (`rom`) is decoded by the server anyway, so normalise asks for it levelled
// there (`?level=1`), quiet tracks turned up as far as their peaks allow.
//
// Crossfade (ms, 2000 to begin with, 0 = off, remembered here too): the next track starts that long before the
// current one ends, fading in while the current one fades out -- also on next / previous. It
// takes a second `<audio>` element for the overlap, faded by its volume like normalise.
//
// Shuffle is on to begin with. Playback starts by itself; where the browser refuses to
// start audio without a click, the bar simply shows "play" and waits.
(function () {
  if (window.rxPlayer) return;

  const CSS = `
    #rx-player-bar { display:flex; align-items:center; gap:6px; padding:4px 8px; margin:0 0 8px; border:1px solid var(--line, #2b3040); border-radius:6px; background:var(--card, #171b26); font-size:13px; }
    #rx-player-bar button { min-width:30px; padding:2px 8px; cursor:pointer; }
    #rx-player-bar button.on { outline:1px solid var(--accent, #6aa3ff); }
    #rx-player-bar .rxp-title { flex:1 1 120px; min-width:60px; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
    #rx-player-bar .rxp-time { font-variant-numeric:tabular-nums; opacity:.75; }
    #rx-player-bar input.rxp-seek { flex:0 1 180px; min-width:60px; }
    #rx-player-bar input.rxp-vol { width:70px; }
    #rx-player-bar label.rxp-norm, #rx-player-bar label.rxp-xf { display:flex; align-items:center; gap:3px; white-space:nowrap; opacity:.85; cursor:pointer; }
    #rx-player-bar label.rxp-xf input { width:64px; }
    #rx-player-list { position:fixed; top:96px; right:0; bottom:0; width:270px; overflow-y:auto; z-index:4; border-left:1px solid var(--line, #2b3040); background:var(--card, #171b26); font-size:12px; }
    #rx-player-list h4 { margin:0; padding:6px 8px; position:sticky; top:0; background:inherit; border-bottom:1px solid var(--line, #2b3040); }
    #rx-player-list div.rxp-row { padding:4px 8px; cursor:pointer; border-bottom:1px solid var(--line, #2b3040); }
    #rx-player-list div.rxp-row:hover { filter:brightness(1.25); }
    #rx-player-list div.rxp-row.now { font-weight:600; border-left:3px solid var(--accent, #6aa3ff); padding-left:5px; }
    #rx-player-list .rxp-sub { opacity:.65; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
    #rx-player-list div.rxp-hrow { display:flex; align-items:center; gap:6px; padding:4px 8px; cursor:pointer; border-bottom:1px solid var(--line, #2b3040); opacity:.7; }
    #rx-player-list div.rxp-hrow:hover { opacity:1; }
    #rx-player-list div.rxp-hrow.now { opacity:1; font-weight:600; border-left:3px solid var(--accent, #6aa3ff); padding-left:5px; }
    #rx-player-list div.rxp-hrow > div { flex:1 1 auto; min-width:0; }
    #rx-player-list div.rxp-hrow button { flex:0 0 auto; font-size:11px; padding:1px 6px; cursor:pointer; }
    body.rx-has-playlist main { margin-right:270px; }
    @media (max-width: 900px) { #rx-player-list { display:none; } body.rx-has-playlist main { margin-right:0; } }
    #rx-player-dock { position:fixed; left:8px; right:8px; bottom:8px; z-index:5; }
    #rx-player-dock #rx-player-bar { margin:0; box-shadow:0 2px 12px rgba(0,0,0,.45); }
    body.rx-docked { padding-bottom:48px; }
  `;

  const TARGET_LUFS = -16;
  // Storage can be missing or blocked (a private window); the player then just starts at
  // full volume, un-normalised, every time.
  const recall = (key, fallback) => { try { const v = localStorage.getItem(key); return v === null ? fallback : v; } catch (e) { return fallback; } };
  const remember = (key, value) => { try { localStorage.setItem(key, String(value)); } catch (e) { /* not remembered */ } };
  const state = { audio: null, tracks: [], order: [], at: 0, shuffle: true, bar: null, list: null, labels: {}, volume: 1, normalise: false, keep: false,
    hidden: [], onHide: null, preview: null, undo: null, crossfade: 0, fadeIn: 1, crossing: false, outgoing: new Set() };
  const FADE_TICK_MS = 40;

  // The factor for one track: 1 unless normalising and the track is louder than the target.
  function gainFor(track) {
    const lufs = track && track.loudness_lufs;
    if (!state.normalise || typeof lufs !== 'number' || !isFinite(lufs)) return 1;
    return Math.min(1, Math.pow(10, (TARGET_LUFS - lufs) / 20));
  }
  function sourceOf(t) {
    if (!t.rom || !state.normalise) return t.url;
    return t.url + (t.url.indexOf('?') < 0 ? '?' : '&') + 'level=1';
  }
  function applyVolume() {
    if (state.audio) state.audio.volume = Math.max(0, Math.min(1, state.volume * gainFor(current()) * state.fadeIn));
  }

  // Ramps `step(f)` from f = 0 to 1 over `ms`, then calls `done`. Returns a cancel function.
  function ramp(ms, each, done) {
    const t0 = Date.now();
    const timer = setInterval(() => {
      const f = Math.min(1, (Date.now() - t0) / Math.max(1, ms));
      each(f);
      if (f >= 1) { clearInterval(timer); if (done) done(); }
    }, FADE_TICK_MS);
    return () => clearInterval(timer);
  }

  // The playing element fades out on its own and is dropped; the caller makes a new one.
  function fadeOut(el, ms) {
    const from = el.volume;
    state.outgoing.add(el);
    ramp(ms, f => { el.volume = Math.max(0, from * (1 - f)); }, () => { el.pause(); el.removeAttribute('src'); el.load(); state.outgoing.delete(el); });
  }
  function dropOutgoing() {
    for (const el of state.outgoing) { el.pause(); el.removeAttribute('src'); }
    state.outgoing.clear();
  }

  // The overlap for this change of track: the setting, but at most half the shorter track.
  function overlapMs() {
    const a = state.audio, t = current();
    if (!state.crossfade || !a || a.paused || !(a.currentTime > 0)) return 0;
    let ms = state.crossfade;
    if (isFinite(a.duration) && a.duration > 0) ms = Math.min(ms, a.duration * 500);
    if (t && t.duration_ms) ms = Math.min(ms, t.duration_ms / 2);
    return Math.max(0, Math.floor(ms));
  }

  const esc = s => String(s == null ? '' : s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const clock = s => { if (!isFinite(s) || s < 0) s = 0; const m = Math.floor(s / 60); return m + ':' + String(Math.floor(s % 60)).padStart(2, '0'); };
  const nameOf = t => t.title || t.file_name || ('#' + t.id);
  const L = k => state.labels[k] || k;

  function makeOrder() {
    state.order = state.tracks.map((_, i) => i);
    if (state.shuffle) {
      for (let i = state.order.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [state.order[i], state.order[j]] = [state.order[j], state.order[i]]; }
    }
  }

  // A hidden track played from the hidden list is playing; otherwise the list's own track.
  function current() { return state.preview || state.tracks[state.order[state.at]]; }

  function renderList() {
    if (!state.list) return;
    const row = (t, i) => `<div>${esc(nameOf(t))}</div><div class="rxp-sub">${esc([t.artist, t.album].filter(Boolean).join(' · '))}${t.duration_ms ? ' · ' + clock(t.duration_ms / 1000) : ''}</div>`;
    let html = `<h4>${esc(L('playlist'))} (${state.tracks.length})</h4>` + state.tracks.map((t, i) => `<div class="rxp-row" data-i="${i}">${row(t, i)}</div>`).join('');
    if (state.hidden.length) {
      html += `<h4>${esc(L('hidden tracks'))} (${state.hidden.length})</h4>` + state.hidden.map((t, i) =>
        `<div class="rxp-hrow" data-h="${i}" title="${esc(L('click to listen'))}"><div>${row(t, i)}</div><button type="button" data-r="${i}">${esc(L('restore'))}</button></div>`).join('');
    }
    state.list.innerHTML = html;
  }

  // "Don't play this track": out of the list now, and recorded by the page. Undo for a while.
  function hideCurrent() {
    const t = current(); if (!t || !state.onHide || t.remote) return;
    if (state.preview) return;
    const i = state.order[state.at];
    state.tracks.splice(i, 1);
    state.order = state.order.filter(j => j !== i).map(j => j > i ? j - 1 : j);
    state.hidden.unshift(t);
    state.onHide(t, true);
    showUndo(t);
    renderList();
    if (!state.tracks.length) { if (state.audio) state.audio.pause(); paint(); return; }
    if (state.at >= state.order.length) state.at = 0;
    load(true);
  }

  function restore(t) {
    const h = state.hidden.indexOf(t); if (h < 0) return;
    state.hidden.splice(h, 1);
    state.tracks.push(t);
    state.order.push(state.tracks.length - 1);
    if (state.onHide) state.onHide(t, false);
    renderList(); paint();
  }

  function showUndo(t) {
    const old = state.bar && state.bar.querySelector('[data-a=undo]'); if (old) old.remove();
    if (state.undo) clearTimeout(state.undo.timer);
    if (!state.bar) return;
    const b = document.createElement('button'); b.type = 'button'; b.dataset.a = 'undo'; b.textContent = L('undo');
    state.bar.querySelector('[data-a=hide]').after(b);
    state.undo = { track: t, timer: setTimeout(() => { b.remove(); state.undo = null; }, 10000) };
  }

  function paint() {
    if (!state.bar) return;
    const t = current(), a = state.audio;
    state.bar.querySelector('.rxp-title').innerHTML = t ? `<b>${esc(nameOf(t))}</b>${t.artist ? ' — ' + esc(t.artist) : ''}${t.album ? ' <span style="opacity:.65">· ' + esc(t.album) + '</span>' : ''}` : '';
    state.bar.querySelector('[data-a=play]').textContent = a && !a.paused ? '⏸' : '▶';
    state.bar.querySelector('[data-a=shuffle]').classList.toggle('on', state.shuffle);
    state.bar.querySelector('[data-a=mute]').classList.toggle('on', !!(a && a.muted));
    state.bar.querySelector('[data-a=mute]').textContent = a && a.muted ? '🔇' : '🔊';
    const hide = state.bar.querySelector('[data-a=hide]');
    if (hide) hide.disabled = !t || !!t.remote || !!state.preview;
    if (state.list) {
      for (const row of state.list.querySelectorAll('.rxp-row')) row.classList.toggle('now', !state.preview && !!t && Number(row.dataset.i) === state.order[state.at]);
      for (const row of state.list.querySelectorAll('.rxp-hrow')) row.classList.toggle('now', !!state.preview && state.hidden[Number(row.dataset.h)] === state.preview);
    }
  }

  function load(autoplay, overlap) {
    const t = current(); if (!t || !state.audio) return;
    state.crossing = false;
    if (autoplay && overlap > 0) {
      // The old element fades out by itself; a fresh one fades in.
      fadeOut(state.audio, overlap);
      state.audio = makeAudio();
      state.fadeIn = 0;
      ramp(overlap, f => { state.fadeIn = f; applyVolume(); });
    } else {
      state.fadeIn = 1;
    }
    state.audio.src = sourceOf(t);
    applyVolume();
    if (autoplay) state.audio.play().catch(() => paint());
    paint();
    const now = state.list && state.list.querySelector('.rxp-row.now');
    if (now && now.scrollIntoView) now.scrollIntoView({ block: 'nearest' });
  }

  function step(delta, autoplay) {
    // After a hidden track played once, the list goes on from where it was.
    if (state.preview) { state.preview = null; delta = Math.max(0, delta); if (delta === 0) { load(autoplay); return; } }
    if (!state.tracks.length) { paint(); return; }
    const overlap = overlapMs();
    state.at = (state.at + delta + state.order.length) % state.order.length;
    load(autoplay, overlap);
  }

  // Set by start(): a new `<audio>` element wired to the bar.
  let makeAudio = () => new Audio();

  function stop() {
    if (state.audio) { state.audio.pause(); state.audio.removeAttribute('src'); state.audio.load(); }
    dropOutgoing();
    if (state.bar) state.bar.remove();
    if (state.list) state.list.remove();
    document.body.classList.remove('rx-has-playlist');
    undock();
    state.audio = null; state.bar = null; state.list = null; state.tracks = []; state.hidden = []; state.preview = null;
  }

  function undock() {
    const dock = document.getElementById('rx-player-dock');
    if (dock) dock.remove();
    document.body.classList.remove('rx-docked');
  }

  // The page is going away: keep the music in a strip at the bottom, or stop it.
  function leave() {
    if (!state.keep || !state.audio || !state.bar || !state.tracks.length) { stop(); return; }
    let dock = document.getElementById('rx-player-dock');
    if (!dock) { dock = document.createElement('div'); dock.id = 'rx-player-dock'; document.body.appendChild(dock); }
    dock.appendChild(state.bar);
    document.body.classList.add('rx-docked');
    // The playlist panel takes a column of the page; the strip is enough on other screens.
    if (state.list) state.list.style.display = 'none';
    document.body.classList.remove('rx-has-playlist');
  }

  function start(host, tracks, labels, options) {
    stop();
    if (!host || !tracks || !tracks.length) return;
    state.keep = !!(options && options.keep);
    state.hidden = (options && options.hidden || []).slice();
    state.onHide = options && typeof options.onHide === 'function' ? options.onHide : null;
    if (!document.getElementById('rx-player-css')) { const s = document.createElement('style'); s.id = 'rx-player-css'; s.textContent = CSS; document.head.appendChild(s); }
    state.labels = labels || {};
    state.tracks = tracks; state.shuffle = true; state.at = 0; makeOrder();
    const stored = parseFloat(recall('rxplayer_volume', '1'));
    state.volume = isFinite(stored) ? Math.max(0, Math.min(1, stored)) : 1;
    state.normalise = recall('rxplayer_normalise', '0') === '1';
    const xf = parseInt(recall('rxplayer_crossfade', '2000'), 10);
    state.crossfade = isFinite(xf) && xf > 0 ? xf : 0;

    const bar = document.createElement('div'); bar.id = 'rx-player-bar';
    bar.innerHTML = `<button type="button" data-a="prev" title="${esc(L('previous'))}">⏮</button>
      <button type="button" data-a="play" title="${esc(L('play / pause'))}">▶</button>
      <button type="button" data-a="stop" title="${esc(L('stop'))}">⏹</button>
      <button type="button" data-a="next" title="${esc(L('next'))}">⏭</button>
      <button type="button" data-a="shuffle" class="on" title="${esc(L('shuffle'))}">🔀</button>
      <button type="button" data-a="mute" title="${esc(L('mute'))}">🔊</button>
      ${state.onHide ? `<button type="button" data-a="hide" title="${esc(L("don't play this track on this page again"))}">🚫</button>` : ''}
      <input type="range" class="rxp-vol" min="0" max="100" value="${Math.round(state.volume * 100)}" title="${esc(L('volume'))}">
      <label class="rxp-norm" title="${esc(L('normalise: play every track at about the same loudness (loud tracks are turned down; quiet ROM tracks are turned up)'))}"><input type="checkbox" ${state.normalise ? 'checked' : ''}>${esc(L('normalise'))}</label>
      <label class="rxp-xf" title="${esc(L('crossfade: the next track fades in while this one fades out, overlapping by this many ms (0 = off)'))}">${esc(L('crossfade'))} <input type="number" min="0" step="100" value="${state.crossfade}"> ms</label>
      <span class="rxp-title"></span>
      <input type="range" class="rxp-seek" min="0" max="1000" value="0" title="${esc(L('position'))}">
      <span class="rxp-time">0:00 / 0:00</span>`;
    host.appendChild(bar);

    const list = document.createElement('div'); list.id = 'rx-player-list';
    document.body.appendChild(list);
    state.list = list; renderList();
    document.body.classList.add('rx-has-playlist');

    state.bar = bar; state.list = list;
    const seek = bar.querySelector('.rxp-seek'), time = bar.querySelector('.rxp-time');
    const volume = bar.querySelector('.rxp-vol'), normalise = bar.querySelector('.rxp-norm input');
    const crossfade = bar.querySelector('.rxp-xf input');
    volume.addEventListener('input', () => { state.volume = volume.value / 100; remember('rxplayer_volume', state.volume); applyVolume(); });
    normalise.addEventListener('change', () => { state.normalise = normalise.checked; remember('rxplayer_normalise', state.normalise ? '1' : '0'); applyVolume(); });
    crossfade.addEventListener('change', () => {
      const v = Math.max(0, Math.floor(Number(crossfade.value) || 0));
      crossfade.value = String(v); state.crossfade = v; remember('rxplayer_crossfade', v);
    });
    let dragging = false;
    // A track that will not load (moved, or a format this browser lacks) is skipped, once
    // round the list at most, so one bad file cannot stall the page.
    let failures = 0;
    // Each element's events count only while it is the one playing: one fading out is done.
    makeAudio = () => {
      const audio = new Audio(); audio.preload = 'metadata';
      const mine = f => (...args) => { if (state.audio === audio) f(...args); };
      audio.addEventListener('ended', mine(() => { if (!state.crossing) step(1, true); }));
      audio.addEventListener('error', mine(() => { if (++failures < state.tracks.length) step(1, true); else paint(); }));
      audio.addEventListener('playing', mine(() => { failures = 0; paint(); }));
      audio.addEventListener('pause', mine(paint));
      audio.addEventListener('timeupdate', mine(() => {
        const d = audio.duration || ((current() || {}).duration_ms || 0) / 1000;
        time.textContent = clock(audio.currentTime) + ' / ' + clock(d);
        if (!dragging && d > 0) seek.value = String(Math.round(audio.currentTime / d * 1000));
        // Near the end: the next track starts now and the two overlap.
        if (!state.crossing && state.crossfade > 0 && isFinite(audio.duration) && audio.duration > 0 && !audio.paused
            && (audio.duration - audio.currentTime) * 1000 <= overlapMs()) {
          state.crossing = true;
          step(1, true);
        }
      }));
      return audio;
    };
    const audio = makeAudio();
    state.audio = audio;
    seek.addEventListener('input', () => { dragging = true; });
    seek.addEventListener('change', () => { const a = state.audio; const d = a && a.duration; if (isFinite(d) && d > 0) a.currentTime = seek.value / 1000 * d; dragging = false; });
    bar.addEventListener('click', e => {
      const a = e.target && e.target.dataset && e.target.dataset.a; if (!a) return;
      const audio = state.audio; if (!audio) return;
      if (a === 'play') { if (audio.paused) audio.play().catch(() => paint()); else audio.pause(); }
      else if (a === 'stop') { dropOutgoing(); audio.pause(); audio.currentTime = 0; }
      else if (a === 'next') step(1, true);
      else if (a === 'prev') step(-1, true);
      else if (a === 'mute') audio.muted = !audio.muted;
      else if (a === 'hide') hideCurrent();
      else if (a === 'undo') { const u = state.undo; if (u) { clearTimeout(u.timer); state.undo = null; e.target.remove(); restore(u.track); } }
      else if (a === 'shuffle') { const keep = state.order[state.at]; state.shuffle = !state.shuffle; makeOrder(); state.at = Math.max(0, state.order.indexOf(keep)); }
      paint();
    });
    list.addEventListener('click', e => {
      const back = e.target.closest && e.target.closest('[data-r]');
      if (back) { restore(state.hidden[Number(back.dataset.r)]); return; }
      // A hidden track plays once, to decide whether to bring it back.
      const hrow = e.target.closest && e.target.closest('.rxp-hrow');
      if (hrow) { const t = state.hidden[Number(hrow.dataset.h)]; if (t) { state.preview = t; load(true); } return; }
      const row = e.target.closest && e.target.closest('.rxp-row'); if (!row) return;
      const i = state.order.indexOf(Number(row.dataset.i)); if (i >= 0) { state.preview = null; state.at = i; load(true); }
    });
    load(true);
  }

  window.rxPlayer = { start, stop, leave };
})();
