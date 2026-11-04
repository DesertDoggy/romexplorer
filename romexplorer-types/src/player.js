// The audio player both UIs use (the web console loads it from /player.js, the desktop app
// evaluates the same text). One `<audio>` element, a thin bar that goes at the top of a
// title / release page, and the playlist as a panel down the right edge of the window.
//
// rxPlayer.start(barHost, tracks, labels, options)
//   barHost: the element the bar is drawn into (the page puts it at its very top)
//   tracks:  [{ id, url, title, artist, album, file_name, duration_ms, loudness_lufs }]
//   labels:  translated text for the buttons (the UI's own language layer supplies them)
//   options: { keep: true } -- the `audio_keep_playing` setting (the playlist says it)
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
    #rx-player-bar label.rxp-norm { display:flex; align-items:center; gap:3px; white-space:nowrap; opacity:.85; cursor:pointer; }
    #rx-player-list { position:fixed; top:96px; right:0; bottom:0; width:270px; overflow-y:auto; z-index:4; border-left:1px solid var(--line, #2b3040); background:var(--card, #171b26); font-size:12px; }
    #rx-player-list h4 { margin:0; padding:6px 8px; position:sticky; top:0; background:inherit; border-bottom:1px solid var(--line, #2b3040); }
    #rx-player-list div.rxp-row { padding:4px 8px; cursor:pointer; border-bottom:1px solid var(--line, #2b3040); }
    #rx-player-list div.rxp-row:hover { filter:brightness(1.25); }
    #rx-player-list div.rxp-row.now { font-weight:600; border-left:3px solid var(--accent, #6aa3ff); padding-left:5px; }
    #rx-player-list .rxp-sub { opacity:.65; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
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
  const state = { audio: null, tracks: [], order: [], at: 0, shuffle: true, bar: null, list: null, labels: {}, volume: 1, normalise: false, keep: false };

  // The factor for one track: 1 unless normalising and the track is louder than the target.
  function gainFor(track) {
    const lufs = track && track.loudness_lufs;
    if (!state.normalise || typeof lufs !== 'number' || !isFinite(lufs)) return 1;
    return Math.min(1, Math.pow(10, (TARGET_LUFS - lufs) / 20));
  }
  function applyVolume() {
    if (state.audio) state.audio.volume = Math.max(0, Math.min(1, state.volume * gainFor(current())));
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

  function current() { return state.tracks[state.order[state.at]]; }

  function paint() {
    if (!state.bar) return;
    const t = current(), a = state.audio;
    state.bar.querySelector('.rxp-title').innerHTML = t ? `<b>${esc(nameOf(t))}</b>${t.artist ? ' — ' + esc(t.artist) : ''}${t.album ? ' <span style="opacity:.65">· ' + esc(t.album) + '</span>' : ''}` : '';
    state.bar.querySelector('[data-a=play]').textContent = a && !a.paused ? '⏸' : '▶';
    state.bar.querySelector('[data-a=shuffle]').classList.toggle('on', state.shuffle);
    state.bar.querySelector('[data-a=mute]').classList.toggle('on', !!(a && a.muted));
    state.bar.querySelector('[data-a=mute]').textContent = a && a.muted ? '🔇' : '🔊';
    if (state.list) for (const row of state.list.querySelectorAll('.rxp-row')) row.classList.toggle('now', !!t && Number(row.dataset.i) === state.order[state.at]);
  }

  function load(autoplay) {
    const t = current(); if (!t || !state.audio) return;
    state.audio.src = t.url;
    applyVolume();
    if (autoplay) state.audio.play().catch(() => paint());
    paint();
    const now = state.list && state.list.querySelector('.rxp-row.now');
    if (now && now.scrollIntoView) now.scrollIntoView({ block: 'nearest' });
  }

  function step(delta, autoplay) {
    if (!state.tracks.length) return;
    state.at = (state.at + delta + state.order.length) % state.order.length;
    load(autoplay);
  }

  function stop() {
    if (state.audio) { state.audio.pause(); state.audio.removeAttribute('src'); state.audio.load(); }
    if (state.bar) state.bar.remove();
    if (state.list) state.list.remove();
    document.body.classList.remove('rx-has-playlist');
    undock();
    state.audio = null; state.bar = null; state.list = null; state.tracks = [];
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
    if (!document.getElementById('rx-player-css')) { const s = document.createElement('style'); s.id = 'rx-player-css'; s.textContent = CSS; document.head.appendChild(s); }
    state.labels = labels || {};
    state.tracks = tracks; state.shuffle = true; state.at = 0; makeOrder();
    const stored = parseFloat(recall('rxplayer_volume', '1'));
    state.volume = isFinite(stored) ? Math.max(0, Math.min(1, stored)) : 1;
    state.normalise = recall('rxplayer_normalise', '0') === '1';

    const bar = document.createElement('div'); bar.id = 'rx-player-bar';
    bar.innerHTML = `<button type="button" data-a="prev" title="${esc(L('previous'))}">⏮</button>
      <button type="button" data-a="play" title="${esc(L('play / pause'))}">▶</button>
      <button type="button" data-a="stop" title="${esc(L('stop'))}">⏹</button>
      <button type="button" data-a="next" title="${esc(L('next'))}">⏭</button>
      <button type="button" data-a="shuffle" class="on" title="${esc(L('shuffle'))}">🔀</button>
      <button type="button" data-a="mute" title="${esc(L('mute'))}">🔊</button>
      <input type="range" class="rxp-vol" min="0" max="100" value="${Math.round(state.volume * 100)}" title="${esc(L('volume'))}">
      <label class="rxp-norm" title="${esc(L('normalise: play every track at about the same loudness (loud tracks are turned down)'))}"><input type="checkbox" ${state.normalise ? 'checked' : ''}>${esc(L('normalise'))}</label>
      <span class="rxp-title"></span>
      <input type="range" class="rxp-seek" min="0" max="1000" value="0" title="${esc(L('position'))}">
      <span class="rxp-time">0:00 / 0:00</span>`;
    host.appendChild(bar);

    const list = document.createElement('div'); list.id = 'rx-player-list';
    list.innerHTML = `<h4>${esc(L('playlist'))} (${tracks.length})</h4>` + tracks.map((t, i) =>
      `<div class="rxp-row" data-i="${i}"><div>${esc(nameOf(t))}</div><div class="rxp-sub">${esc([t.artist, t.album].filter(Boolean).join(' · '))}${t.duration_ms ? ' · ' + clock(t.duration_ms / 1000) : ''}</div></div>`).join('');
    document.body.appendChild(list);
    document.body.classList.add('rx-has-playlist');

    const audio = new Audio(); audio.preload = 'metadata';
    state.audio = audio; state.bar = bar; state.list = list;
    const seek = bar.querySelector('.rxp-seek'), time = bar.querySelector('.rxp-time');
    const volume = bar.querySelector('.rxp-vol'), normalise = bar.querySelector('.rxp-norm input');
    volume.addEventListener('input', () => { state.volume = volume.value / 100; remember('rxplayer_volume', state.volume); applyVolume(); });
    normalise.addEventListener('change', () => { state.normalise = normalise.checked; remember('rxplayer_normalise', state.normalise ? '1' : '0'); applyVolume(); });
    let dragging = false;
    audio.addEventListener('ended', () => step(1, true));
    // A track that will not load (moved, or a format this browser lacks) is skipped, once
    // round the list at most, so one bad file cannot stall the page.
    let failures = 0;
    audio.addEventListener('error', () => { if (++failures < state.tracks.length) step(1, true); else paint(); });
    audio.addEventListener('playing', () => { failures = 0; paint(); });
    audio.addEventListener('pause', paint);
    audio.addEventListener('timeupdate', () => {
      const d = audio.duration || ((current() || {}).duration_ms || 0) / 1000;
      time.textContent = clock(audio.currentTime) + ' / ' + clock(d);
      if (!dragging && d > 0) seek.value = String(Math.round(audio.currentTime / d * 1000));
    });
    seek.addEventListener('input', () => { dragging = true; });
    seek.addEventListener('change', () => { const d = audio.duration; if (isFinite(d) && d > 0) audio.currentTime = seek.value / 1000 * d; dragging = false; });
    bar.addEventListener('click', e => {
      const a = e.target && e.target.dataset && e.target.dataset.a; if (!a) return;
      if (a === 'play') { if (audio.paused) audio.play().catch(() => paint()); else audio.pause(); }
      else if (a === 'stop') { audio.pause(); audio.currentTime = 0; }
      else if (a === 'next') step(1, true);
      else if (a === 'prev') step(-1, true);
      else if (a === 'mute') audio.muted = !audio.muted;
      else if (a === 'shuffle') { const keep = state.order[state.at]; state.shuffle = !state.shuffle; makeOrder(); state.at = Math.max(0, state.order.indexOf(keep)); }
      paint();
    });
    list.addEventListener('click', e => {
      const row = e.target.closest && e.target.closest('.rxp-row'); if (!row) return;
      const i = state.order.indexOf(Number(row.dataset.i)); if (i >= 0) { state.at = i; load(true); }
    });
    load(true);
  }

  window.rxPlayer = { start, stop, leave };
})();
