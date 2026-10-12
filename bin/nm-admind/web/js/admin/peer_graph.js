import { esc, fmtBytes, fmtLoss, fmtRate, fmtRtt, pathKindLabel, sampleRate, shortId } from "./helpers.js";

/**
 * 对等拓扑实时图：本节点居中，对等环绕；边表示会话，粒子表示传输，色调表示 RTT。
 */
export class PeerGraph {
  constructor(wrap) {
    this.wrap = wrap;
    this.cv = wrap.querySelector("canvas");
    this.tip = wrap.querySelector(".peer-map__tip");
    this.ctx = this.cv.getContext("2d");
    this.selfId = "";
    this.federation = "";
    this.peers = [];
    this.nodes = new Map(); // id -> {x,y,vx,vy,...}
    this.particles = [];
    this.hoverId = null;
    this.t0 = performance.now();
    this._ro = new ResizeObserver(() => this._resize());
    this._ro.observe(wrap);
    this._onMove = (e) => this._pointer(e);
    this._onLeave = () => { this.hoverId = null; this.tip.classList.remove("is-on"); };
    this.cv.addEventListener("pointermove", this._onMove);
    this.cv.addEventListener("pointerleave", this._onLeave);
    this._resize();
    this._raf = requestAnimationFrame((t) => this._frame(t));
  }
  setData({ selfId, federation, peers }) {
    this.selfId = selfId || "";
    this.federation = federation || "";
    // 刷新时采样一次吞吐，动画帧只读缓存，避免污染差分窗口。
    this.peers = (peers || []).map((p) => {
      const rate = p.connected ? sampleRate(p.id, p.bytes_rx, p.bytes_tx) : { down: null, up: null };
      return { ...p, _rate: rate };
    });
    const ids = new Set(this.peers.map((p) => p.id));
    ids.add("__self__");
    for (const id of [...this.nodes.keys()]) {
      if (!ids.has(id)) this.nodes.delete(id);
    }
    if (!this.nodes.has("__self__")) {
      this.nodes.set("__self__", { x: 0, y: 0, vx: 0, vy: 0, r: 22 });
    }
    const n = this.peers.length || 1;
    this.peers.forEach((p, i) => {
      if (!this.nodes.has(p.id)) {
        const ang = (i / n) * Math.PI * 2 - Math.PI / 2;
        const rad = 0.62;
        this.nodes.set(p.id, {
          x: Math.cos(ang) * rad,
          y: Math.sin(ang) * rad,
          vx: 0, vy: 0, r: 16,
        });
      }
    });
  }
  _resize() {
    const dpr = window.devicePixelRatio || 1;
    const w = this.wrap.clientWidth || 600;
    const h = this.wrap.clientHeight || 360;
    this.w = w; this.h = h;
    this.cv.width = Math.floor(w * dpr);
    this.cv.height = Math.floor(h * dpr);
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  _xy(n) {
    return { x: this.w / 2 + n.x * Math.min(this.w, this.h) * 0.42, y: this.h / 2 + n.y * Math.min(this.w, this.h) * 0.42 };
  }
  _pointer(e) {
    const rect = this.cv.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    let hit = null, best = 1e9;
    for (const [id, n] of this.nodes) {
      const p = this._xy(n);
      const d = Math.hypot(p.x - x, p.y - y);
      if (d < n.r + 8 && d < best) { best = d; hit = id; }
    }
    this.hoverId = hit;
    if (!hit || hit === "__self__") {
      if (hit === "__self__") {
        this.tip.innerHTML = `<b>本节点</b><div class="mono">${esc(shortId(this.selfId))}</div>
          <div class="row"><span>联邦</span><span>${esc(this.federation)}</span></div>
          <div class="row"><span>对等</span><span>${this.peers.length}</span></div>`;
        this._placeTip(e.clientX - rect.left, e.clientY - rect.top);
      } else this.tip.classList.remove("is-on");
      return;
    }
    const peer = this.peers.find((p) => p.id === hit);
    if (!peer) { this.tip.classList.remove("is-on"); return; }
    const rate = peer._rate || { down: null, up: null };
    this.tip.innerHTML = `<b>${esc(peer.name) || "未命名节点"}</b>
      <div class="mono">${esc(shortId(peer.id))}</div>
      <div class="row"><span>状态</span><span>${peer.connected ? "在线" : "离线"}</span></div>
      <div class="row"><span>来源</span><span>${peer.source === "discovered" ? "自动发现" : "手工"}</span></div>
      <div class="row"><span>路径</span><span>${peer.connected ? pathKindLabel(peer.path_kind) : "—"}</span></div>
      <div class="row"><span>RTT</span><span>${peer.connected ? fmtRtt(peer.rtt_ms) : "—"}</span></div>
      <div class="row"><span>丢包</span><span>${peer.connected ? fmtLoss(peer.lost_packets, peer.datagrams_tx) : "—"}</span></div>
      <div class="row"><span>带宽</span><span>${peer.connected ? `${fmtRate(rate.down)} ↓ / ${fmtRate(rate.up)} ↑` : "—"}</span></div>
      <div class="row"><span>流量</span><span>${peer.connected ? `↓${fmtBytes(peer.bytes_rx)} ↑${fmtBytes(peer.bytes_tx)}` : "—"}</span></div>`;
    this._placeTip(x, y);
  }
  _placeTip(x, y) {
    this.tip.classList.add("is-on");
    const tw = this.tip.offsetWidth || 200, th = this.tip.offsetHeight || 120;
    let left = x + 14, top = y + 14;
    if (left + tw > this.w - 8) left = x - tw - 10;
    if (top + th > this.h - 8) top = y - th - 10;
    this.tip.style.left = Math.max(6, left) + "px";
    this.tip.style.top = Math.max(6, top) + "px";
  }
  _linkQuality(p) {
    if (!p.connected) return { color: "rgba(138,148,166,.35)", width: 1.2, speed: 0, glow: 0, rate: { down: 0, up: 0 } };
    const rtt = Number(p.rtt_ms);
    let color = "rgba(18,160,106,.85)"; // good
    if (Number.isFinite(rtt)) {
      if (rtt > 180) color = "rgba(229,72,77,.9)";
      else if (rtt > 80) color = "rgba(245,158,11,.9)";
    }
    if (p.path_kind === "relay") color = "rgba(14,165,233,.9)";
    const rate = p._rate || { down: 0, up: 0 };
    const bps = Math.max(rate.down || 0, rate.up || 0);
    const width = 1.5 + Math.min(7, Math.log10(1 + bps / 512) * 3.2);
    const speed = 0.15 + Math.min(1.6, bps / (80 * 1024));
    return { color, width, speed, glow: Math.min(1, bps / (200 * 1024)), rate };
  }
  _stepPhysics(dt) {
    const self = this.nodes.get("__self__");
    if (!self) return;
    self.x *= 0.85; self.y *= 0.85;
    const list = this.peers.map((p) => this.nodes.get(p.id)).filter(Boolean);
    // soft circular target + repulsion
    const n = Math.max(list.length, 1);
    list.forEach((a, i) => {
      const ang = (i / n) * Math.PI * 2 - Math.PI / 2 + (this.t0 % 100000) * 0.00002;
      const tx = Math.cos(ang) * 0.68, ty = Math.sin(ang) * 0.68;
      a.vx += (tx - a.x) * 1.8 * dt;
      a.vy += (ty - a.y) * 1.8 * dt;
    });
    for (let i = 0; i < list.length; i++) {
      for (let j = i + 1; j < list.length; j++) {
        const a = list[i], b = list[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let d2 = dx * dx + dy * dy || 0.0001;
        if (d2 < 0.09) {
          const f = (0.09 - d2) * 2.2;
          const inv = 1 / Math.sqrt(d2);
          dx *= inv; dy *= inv;
          a.vx += dx * f * dt; a.vy += dy * f * dt;
          b.vx -= dx * f * dt; b.vy -= dy * f * dt;
        }
      }
    }
    for (const a of list) {
      a.vx *= 0.86; a.vy *= 0.86;
      a.x += a.vx * dt; a.y += a.vy * dt;
      const lim = 0.92;
      const m = Math.hypot(a.x, a.y);
      if (m > lim) { a.x *= lim / m; a.y *= lim / m; }
    }
  }
  _spawnParticles(dt) {
    const self = this.nodes.get("__self__");
    if (!self) return;
    const sxy = this._xy(self);
    for (const p of this.peers) {
      if (!p.connected) continue;
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const q = this._linkQuality(p);
      if (q.speed <= 0) continue;
      // spawn rate ~ bandwidth
      const chance = q.speed * dt * 2.2;
      if (Math.random() > chance) continue;
      const rate = q.rate || { down: 0, up: 0 };
      const toPeer = (rate.up || 0) >= (rate.down || 0);
      const pxy = this._xy(n);
      this.particles.push({
        x0: toPeer ? sxy.x : pxy.x,
        y0: toPeer ? sxy.y : pxy.y,
        x1: toPeer ? pxy.x : sxy.x,
        y1: toPeer ? pxy.y : sxy.y,
        t: 0,
        speed: 0.55 + q.speed * 0.7,
        color: q.color,
        size: 2 + Math.min(3.5, q.width * 0.35),
      });
    }
    if (this.particles.length > 220) this.particles.splice(0, this.particles.length - 220);
  }
  _frame(now) {
    if (!this.cv.isConnected) {
      this._ro.disconnect();
      this.cv.removeEventListener("pointermove", this._onMove);
      this.cv.removeEventListener("pointerleave", this._onLeave);
      return;
    }
    const dt = Math.min(0.05, (now - (this._last || now)) / 1000) || 0.016;
    this._last = now;
    this._stepPhysics(dt);
    this._spawnParticles(dt);
    const ctx = this.ctx;
    ctx.clearRect(0, 0, this.w, this.h);
    // soft grid
    ctx.save();
    ctx.strokeStyle = getComputedStyle(document.body).getPropertyValue("--border").trim() || "#e6e9f0";
    ctx.globalAlpha = 0.45;
    ctx.lineWidth = 1;
    const step = 36;
    for (let x = step; x < this.w; x += step) { ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, this.h); ctx.stroke(); }
    for (let y = step; y < this.h; y += step) { ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(this.w, y); ctx.stroke(); }
    ctx.restore();

    const self = this.nodes.get("__self__");
    const sxy = self ? this._xy(self) : { x: this.w / 2, y: this.h / 2 };

    // links
    for (const p of this.peers) {
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const pxy = this._xy(n);
      const q = this._linkQuality(p);
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(sxy.x, sxy.y);
      ctx.lineTo(pxy.x, pxy.y);
      ctx.strokeStyle = q.color;
      ctx.lineWidth = q.width;
      if (!p.connected) ctx.setLineDash([5, 6]);
      ctx.globalAlpha = p.connected ? 0.85 : 0.55;
      if (q.glow > 0.05) {
        ctx.shadowColor = q.color;
        ctx.shadowBlur = 8 + q.glow * 14;
      }
      ctx.stroke();
      ctx.restore();
      // edge label
      if (p.connected) {
        const mx = (sxy.x + pxy.x) / 2, my = (sxy.y + pxy.y) / 2;
        const label = `${fmtRtt(p.rtt_ms)}`;
        ctx.save();
        ctx.font = "600 10px ui-sans-serif, system-ui, sans-serif";
        ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
        ctx.textAlign = "center";
        ctx.fillText(label, mx, my - 6);
        ctx.restore();
      }
    }

    // particles
    const alive = [];
    for (const pt of this.particles) {
      pt.t += dt * pt.speed;
      if (pt.t > 1) continue;
      const x = pt.x0 + (pt.x1 - pt.x0) * pt.t;
      const y = pt.y0 + (pt.y1 - pt.y0) * pt.t;
      ctx.beginPath();
      ctx.fillStyle = pt.color;
      ctx.globalAlpha = 0.35 + 0.65 * Math.sin(pt.t * Math.PI);
      ctx.arc(x, y, pt.size, 0, Math.PI * 2);
      ctx.fill();
      ctx.globalAlpha = 1;
      alive.push(pt);
    }
    this.particles = alive;

    // peer nodes
    for (const p of this.peers) {
      const n = this.nodes.get(p.id);
      if (!n) continue;
      const pxy = this._xy(n);
      const on = !!p.connected;
      const pulse = on ? 1 + 0.08 * Math.sin(now / 280) : 1;
      const r = n.r * pulse;
      ctx.beginPath();
      ctx.fillStyle = on
        ? (p.path_kind === "relay" ? "#0ea5e9" : "#12a06a")
        : "#8a94a6";
      ctx.globalAlpha = on ? 1 : 0.55;
      ctx.arc(pxy.x, pxy.y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.lineWidth = this.hoverId === p.id ? 3 : 1.5;
      ctx.strokeStyle = getComputedStyle(document.body).getPropertyValue("--surface").trim() || "#fff";
      ctx.stroke();
      // label
      const name = p.name || shortId(p.id);
      ctx.font = "600 11px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--text").trim() || "#101828";
      ctx.fillText(name.length > 12 ? name.slice(0, 11) + "…" : name, pxy.x, pxy.y + r + 14);
      if (on) {
        const rate = p._rate || { down: null, up: null };
        const bps = Math.max(rate.down || 0, rate.up || 0);
        if (bps > 0) {
          ctx.font = "500 10px ui-sans-serif, system-ui, sans-serif";
          ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
          ctx.fillText(fmtRate(bps), pxy.x, pxy.y + r + 26);
        }
      }
    }

    // self node
    if (self) {
      const pulse = 1 + 0.06 * Math.sin(now / 320);
      const r = self.r * pulse;
      const g = ctx.createRadialGradient(sxy.x, sxy.y, 2, sxy.x, sxy.y, r * 2.2);
      g.addColorStop(0, "rgba(91,91,240,.55)");
      g.addColorStop(1, "rgba(91,91,240,0)");
      ctx.beginPath();
      ctx.fillStyle = g;
      ctx.arc(sxy.x, sxy.y, r * 2.2, 0, Math.PI * 2);
      ctx.fill();
      ctx.beginPath();
      ctx.fillStyle = "#5b5bf0";
      ctx.arc(sxy.x, sxy.y, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.lineWidth = this.hoverId === "__self__" ? 3 : 2;
      ctx.strokeStyle = "#fff";
      ctx.stroke();
      ctx.font = "700 12px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--text").trim() || "#101828";
      ctx.fillText("本节点", sxy.x, sxy.y + r + 16);
      ctx.font = "500 10px ui-monospace, Menlo, monospace";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
      ctx.fillText(shortId(this.selfId) || "—", sxy.x, sxy.y + r + 30);
    }

    if (!this.peers.length) {
      ctx.font = "500 13px ui-sans-serif, system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.fillStyle = getComputedStyle(document.body).getPropertyValue("--muted").trim() || "#8a94a6";
      ctx.fillText("暂无对等节点 — 添加种子或等待自动发现", this.w / 2, this.h / 2 + 56);
    }

    this._raf = requestAnimationFrame((t) => this._frame(t));
  }
}
