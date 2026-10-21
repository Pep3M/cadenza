// Cadenza device visual helpers: geometry for displays. Exposed as window.DV.
(function () {
  const L = Math.log10, PI = Math.PI;
  const DV = {};
  const r1 = n => Math.round(n * 10) / 10;
  DV.rand = seed => () => { seed |= 0; seed = seed + 0x6D2B79F5 | 0; let t = Math.imul(seed ^ seed >>> 15, 1 | seed); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; };
  DV.fx = (f, W) => W * L(f / 20) / L(1000);
  DV.xf = (x, W) => 20 * Math.pow(1000, x / W);
  DV.fy = (db, H, R) => H / 2 - db / R * (H / 2);
  DV.freqs = n => Array.from({ length: n }, (_, i) => 20 * Math.pow(1000, i / (n - 1)));
  const FS = 96000;
  function biq(type, f0, g, Q) {
    const A = Math.pow(10, g / 40), w0 = 2 * PI * f0 / FS, c = Math.cos(w0), s = Math.sin(w0), al = s / (2 * Q);
    let b0, b1, b2, a0, a1, a2;
    if (type === 'peak') { b0 = 1 + al * A; b1 = -2 * c; b2 = 1 - al * A; a0 = 1 + al / A; a1 = -2 * c; a2 = 1 - al / A; }
    else if (type === 'ls') { const q = 2 * Math.sqrt(A) * al; b0 = A * ((A + 1) - (A - 1) * c + q); b1 = 2 * A * ((A - 1) - (A + 1) * c); b2 = A * ((A + 1) - (A - 1) * c - q); a0 = (A + 1) + (A - 1) * c + q; a1 = -2 * ((A - 1) + (A + 1) * c); a2 = (A + 1) + (A - 1) * c - q; }
    else if (type === 'hs') { const q = 2 * Math.sqrt(A) * al; b0 = A * ((A + 1) + (A - 1) * c + q); b1 = -2 * A * ((A - 1) + (A + 1) * c); b2 = A * ((A + 1) + (A - 1) * c - q); a0 = (A + 1) - (A - 1) * c + q; a1 = 2 * ((A - 1) - (A + 1) * c); a2 = (A + 1) - (A - 1) * c - q; }
    else if (type === 'hp') { b0 = (1 + c) / 2; b1 = -(1 + c); b2 = (1 + c) / 2; a0 = 1 + al; a1 = -2 * c; a2 = 1 - al; }
    else { b0 = (1 - c) / 2; b1 = 1 - c; b2 = (1 - c) / 2; a0 = 1 + al; a1 = -2 * c; a2 = 1 - al; }
    return [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
  }
  function mag(co, f) {
    const w = 2 * PI * f / FS, c1 = Math.cos(w), c2 = Math.cos(2 * w), s1 = Math.sin(w), s2 = Math.sin(2 * w);
    const nr = co[0] + co[1] * c1 + co[2] * c2, ni = -(co[1] * s1 + co[2] * s2);
    const dr = 1 + co[3] * c1 + co[4] * c2, di = -(co[3] * s1 + co[4] * s2);
    return 10 * L((nr * nr + ni * ni) / (dr * dr + di * di));
  }
  // band: {type:'hp'|'ls'|'peak'|'hs'|'lp', f, g, q, slope}
  DV.bandDb = (b, f) => {
    if (b.type === 'hp' || b.type === 'lp') { if (!b.slope) return 0; return mag(biq(b.type, b.f, 0, 0.707), f) * (b.slope / 12); }
    if (!b.g) return 0;
    return mag(biq(b.type, b.f, b.g, b.type === 'peak' ? b.q : 0.707), f);
  };
  DV.sumDb = (bands, f, out = 0) => bands.reduce((a, b) => a + DV.bandDb(b, f), out);
  DV.pts = (fn, W, H, R, n = 220, y0 = 0) => DV.freqs(n).map(f => [DV.fx(f, W), Math.max(-40, Math.min(H + 40, DV.fy(fn(f), H, R))) + y0]);
  DV.line = pts => pts.map((p, i) => (i ? 'L' : 'M') + r1(p[0]) + ' ' + r1(p[1])).join(' ');
  DV.area = (pts, base) => DV.line(pts) + ` L${r1(pts[pts.length - 1][0])} ${base} L${r1(pts[0][0])} ${base} Z`;
  // analyzer spectrum in dBFS (0..-90), y mapped into H
  DV.spectrum = (seed, W, H, o = {}) => {
    const rnd = DV.rand(seed), n = o.n || 160, tilt = o.tilt ?? 0, lvl = o.level ?? 0;
    let s = 0;
    return DV.freqs(n).map(f => {
      s = s * 0.62 + (rnd() - 0.5) * 9 * 0.38;
      const lo = f < 60 ? -14 * (1 - f / 60) : 0;
      let db = -26 + lvl - 5.2 * Math.max(0, Math.log2(f / 180)) + lo + s + (o.shape ? o.shape(f) : 0) + tilt * L(f / 1000) * 6;
      if (f > 16000) db -= (f - 16000) / 400;
      return [DV.fx(f, W), Math.min(H, Math.max(0, -db / 90 * H))];
    });
  };
  DV.pt = (cx, cy, r, a) => [cx + r * Math.sin(a * PI / 180), cy - r * Math.cos(a * PI / 180)];
  DV.arc = (cx, cy, r, a0, a1) => {
    if (Math.abs(a1 - a0) < 0.5) return '';
    const s = Math.min(a0, a1), e = Math.max(a0, a1), p0 = DV.pt(cx, cy, r, s), p1 = DV.pt(cx, cy, r, e);
    return `M${r1(p0[0])} ${r1(p0[1])} A${r} ${r} 0 ${e - s > 180 ? 1 : 0} 1 ${r1(p1[0])} ${r1(p1[1])}`;
  };
  // knob geometry. v 0..1. size S=28 M=40 L=64 XL=96
  DV.knob = (v, o = {}) => {
    const s = o.size || 40, c = s / 2, sw = s >= 64 ? 4 : s >= 40 ? 3 : 2.5, rr = c - sw / 2 - 1;
    const a = -135 + 270 * v, from = o.bip ? 0 : -135;
    const p1 = DV.pt(c, c, rr - sw - 2, a), p0 = DV.pt(c, c, (rr - sw) * 0.25, a);
    const k = { s, c, sw, rr, br: rr - sw - 1.5, track: DV.arc(c, c, rr, -135, 135), val: DV.arc(c, c, rr, from, a), x1: r1(p0[0]), y1: r1(p0[1]), x2: r1(p1[0]), y2: r1(p1[1]) };
    if (o.auto != null) { const aa = -135 + 270 * o.auto, ap = DV.pt(c, c, rr, aa); k.ax = r1(ap[0]); k.ay = r1(ap[1]); }
    if (o.mod != null) { k.mod = DV.arc(c, c, rr + sw / 2 + 2, a, -135 + 270 * Math.min(1, Math.max(0, v + o.mod))); }
    return k;
  };
  // ADSR: times as fractions of W, sustain 0..1, curve power
  DV.adsr = (wa, wd, sus, wr, W, H, o = {}) => {
    const pad = o.pad ?? 6, top = pad, bot = H - pad, hold = o.hold ?? 0.18;
    const x0 = pad, xa = x0 + wa * W, xd = xa + wd * W, xs = xd + hold * W, xr = xs + wr * W, ys = bot - (bot - top) * sus;
    const seg = (xA, yA, xB, yB, p) => Array.from({ length: 16 }, (_, i) => { const t = (i + 1) / 16, e = 1 - Math.pow(1 - t, p); return [xA + (xB - xA) * t, yA + (yB - yA) * e]; });
    const P = [[x0, bot], ...seg(x0, bot, xa, top, 1.6), ...seg(xa, top, xd, ys, 3), [xs, ys], ...seg(xs, ys, xr, bot, 3)];
    return { line: DV.line(P), fill: DV.area(P, bot), a: [r1(xa), top], d: [r1(xd), r1(ys)], s: [r1(xs), r1(ys)], r: [r1(xr), bot], bot, top };
  };
  DV.wave = (type, W, H, cyc = 2, o = {}) => {
    const n = o.n || 160, A = (H / 2) * (o.amp ?? 0.8), cy = (o.y0 ?? 0) + H / 2;
    const f = t => { const p = (t * cyc + (o.phase || 0)) % 1; if (type === 'sine') return Math.sin(2 * PI * p); if (type === 'saw') return 1 - 2 * p; if (type === 'square') return p < 0.5 ? 1 : -1; return p < 0.5 ? 4 * p - 1 : 3 - 4 * p; };
    const P = []; for (let i = 0; i <= n; i++) { const t = i / n; let v = f(t); if (o.shape) v = o.shape(v); if (type === 'square' && i > 0) { const pv = f((i - 1) / n); if (Math.sign(pv) !== Math.sign(v)) P.push([(o.x0 || 0) + t * W, cy - A * (o.shape ? o.shape(pv) : pv)]); } P.push([(o.x0 || 0) + t * W, cy - A * v]); }
    return DV.line(P);
  };
  // decaying waveform as mirrored area, env(t) 0..1
  DV.waveArea = (seed, W, H, env, o = {}) => {
    const rnd = DV.rand(seed), n = o.n || 300, cy = (o.y0 || 0) + H / 2, top = [], bot = [];
    for (let i = 0; i <= n; i++) { const t = i / n, a = env(t) * (0.55 + 0.45 * rnd()) * H / 2; top.push([(o.x0 || 0) + t * W, cy - a]); bot.push([(o.x0 || 0) + t * W, cy + a * (0.85 + 0.15 * rnd())]); }
    return DV.line(top.concat(bot.reverse())) + ' Z';
  };
  DV.bars = (seed, n, env) => { const rnd = DV.rand(seed); return Array.from({ length: n }, (_, i) => Math.round(Math.max(2, env(i / n) * (0.5 + 0.5 * rnd()) * 100))); };
  const NN = ['C', 'C♯', 'D', 'D♯', 'E', 'F', 'F♯', 'G', 'G♯', 'A', 'A♯', 'B'];
  DV.note = f => { const m = 69 + 12 * Math.log2(f / 440), r = Math.round(m), c = Math.round((m - r) * 100); return NN[(r % 12 + 12) % 12] + (Math.floor(r / 12) - 1) + (c ? (c > 0 ? ' +' : ' −') + Math.abs(c) + 'c' : ''); };
  DV.hz = f => f >= 1000 ? (f / 1000).toFixed(f >= 10000 ? 1 : 2).replace(/\.?0+$/, '') + ' kHz' : Math.round(f) + ' Hz';
  DV.db = (d, p = 1) => (d > 0 ? '+' : d < 0 ? '−' : '') + Math.abs(d).toFixed(p) + ' dB';
  DV.grid = (W, H, R, step) => {
    const fl = [20, 30, 40, 50, 60, 70, 80, 90, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 2000, 3000, 4000, 5000, 6000, 7000, 8000, 9000, 10000, 20000];
    const major = { 20: '20', 50: '50', 100: '100', 200: '200', 500: '500', 1000: '1k', 2000: '2k', 5000: '5k', 10000: '10k', 20000: '20k' };
    const v = fl.map(f => ({ x: r1(DV.fx(f, W)), major: !!major[f], label: major[f] || '', c: major[f] ? '#1c1e22' : '#15171a' }));
    const h = []; for (let d = -R; d <= R; d += step) h.push({ y: r1(DV.fy(d, H, R)), label: (d > 0 ? '+' : d < 0 ? '−' : '') + Math.abs(d), zero: d === 0, c: d === 0 ? '#2a2c32' : '#17191c' });
    return { v, h };
  };
  window.DV = DV;
})();
