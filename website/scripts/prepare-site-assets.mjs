import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const scriptPath = fileURLToPath(import.meta.url);
const websiteRoot = path.resolve(path.dirname(scriptPath), '..');

const bannerSvgPath = path.join(websiteRoot, 'public', 'repo_banner.svg');
const generatedDir = path.join(websiteRoot, 'src', 'assets', 'generated');
const posterOutPath = path.join(generatedDir, 'repo_banner_poster.png');

const katexDistDir = path.join(websiteRoot, 'node_modules', 'katex', 'dist');
const katexPublicDir = path.join(websiteRoot, 'public', 'katex');

function isUpToDate(targetPath, inputPaths) {
  if (!fs.existsSync(targetPath)) {
    return false;
  }
  const targetMtime = fs.statSync(targetPath).mtimeMs;
  return inputPaths.every((p) => fs.existsSync(p) && fs.statSync(p).mtimeMs <= targetMtime);
}

function sampleCubicBezier(x1, y1, x2, y2, xTarget) {
  if (xTarget <= 0) return 0;
  if (xTarget >= 1) return 1;
  const cx = 3 * x1;
  const bx = 3 * (x2 - x1) - cx;
  const ax = 1 - cx - bx;
  const cy = 3 * y1;
  const by = 3 * (y2 - y1) - cy;
  const ay = 1 - cy - by;

  const sampleX = (t) => ((ax * t + bx) * t + cx) * t;
  const sampleDerivX = (t) => (3 * ax * t + 2 * bx) * t + cx;
  const sampleY = (t) => ((ay * t + by) * t + cy) * t;

  let t = xTarget;
  for (let i = 0; i < 8; i++) {
    const err = sampleX(t) - xTarget;
    if (Math.abs(err) < 1e-7) return sampleY(t);
    const d = sampleDerivX(t);
    if (Math.abs(d) < 1e-7) break;
    t -= err / d;
  }

  let lo = 0;
  let hi = 1;
  t = xTarget;
  for (let i = 0; i < 16; i++) {
    const val = sampleX(t);
    if (Math.abs(val - xTarget) < 1e-7) break;
    if (xTarget > val) lo = t;
    else hi = t;
    t = 0.5 * (lo + hi);
  }
  return sampleY(t);
}

function parseKeyframesMap(cssText) {
  const keyframes = new Map();
  const kfRe = /@keyframes\s+([\w-]+)\s*\{((?:[^{}]*\{[^{}]*\})*[^{}]*)\}/g;
  for (const m of cssText.matchAll(kfRe)) {
    const name = m[1];
    const body = m[2];
    const stops = [];
    let propKind = null;
    const stopRe = /([0-9.,%\s]+)\{([^{}]*)\}/g;
    for (const sm of body.matchAll(stopRe)) {
      const sel = sm[1];
      const decl = sm[2];
      let val = null;
      const scaleMatch = decl.match(/scaleY\(\s*([+-]?\d*\.?\d+)\s*\)/);
      const opMatch = decl.match(/opacity\s*:\s*([+-]?\d*\.?\d+)/);
      if (scaleMatch) {
        propKind = 'scaleY';
        val = Number.parseFloat(scaleMatch[1]);
      } else if (opMatch) {
        propKind = 'opacity';
        val = Number.parseFloat(opMatch[1]);
      }
      if (val === null) continue;
      for (const pctTok of sel.split(',')) {
        const pct = Number.parseFloat(pctTok.trim());
        if (!Number.isNaN(pct)) {
          stops.push({ p: pct / 100, v: val });
        }
      }
    }
    stops.sort((a, b) => a.p - b.p);
    if (propKind && stops.length >= 2) {
      keyframes.set(name, { propKind, stops });
    }
  }
  return keyframes;
}

function evalCalcSeconds(expr, vars) {
  let resolved = expr.replace(/var\(\s*(--[\w-]+)\s*(?:,\s*([+-]?\d*\.?\d+))?\s*\)/g, (_, name, fallback) => {
    return String(vars[name] ?? (fallback !== undefined ? Number.parseFloat(fallback) : 1));
  });
  resolved = resolved.replace(/^calc\(([\s\S]*)\)$/i, '$1').replace(/s\b/g, '').trim();
  if (!/^[0-9.+\-*/()\s]+$/.test(resolved)) {
    throw new Error(`Unsupported CSS time expression: ${expr} -> ${resolved}`);
  }
  const val = Function(`"use strict"; return (${resolved});`)();
  if (!Number.isFinite(val)) {
    throw new Error(`Non-finite CSS time expression: ${expr} -> ${val}`);
  }
  return val;
}

function bakeInitialCssFrameForLibrsvg(svgText) {
  const styleMatch = svgText.match(/<style[^>]*>([\s\S]*?)<\/style>/i);
  if (!styleMatch) return svgText;
  const cssText = styleMatch[1].replace(/\/\*[\s\S]*?\*\//g, '');

  const keyframes = parseKeyframesMap(cssText);
  const winVars = new Map();
  for (const wm of cssText.matchAll(/#(cspa-win-\d+)\s*\{([^}]*)\}/g)) {
    const vars = { '--seed': 1, '--speed': 1, '--spike-gain': 1, '--flip-x': 1, '--flip-y': 1 };
    for (const vm of wm[2].matchAll(/(--[\w-]+)\s*:\s*([+-]?\d*\.?\d+)/g)) {
      vars[vm[1]] = Number.parseFloat(vm[2]);
    }
    winVars.set(wm[1], vars);
  }

  const winClassValues = new Map();
  let matchedSpikeRules = 0;
  const ruleRe = /#(cspa-win-\d+)\s+\.([\w-]+)\s*\{([^}]*)\}/g;
  for (const rm of cssText.matchAll(ruleRe)) {
    const winId = rm[1];
    const cls = rm[2];
    const decl = rm[3];
    const animMatch = decl.match(
      /animation\s*:\s*([\w-]+)\s+(calc\((?:[^()]+|\([^()]*\))*\)|[+-]?\d*\.?\d+s)\s+(ease-in-out|cubic-bezier\([^)]+\))/i
    );
    if (!animMatch) continue;
    const kf = keyframes.get(animMatch[1]);
    if (!kf) continue;

    const vars = winVars.get(winId) ?? { '--seed': 1, '--speed': 1, '--spike-gain': 1, '--flip-x': 1, '--flip-y': 1 };
    const dur = evalCalcSeconds(animMatch[2], vars);
    if (!(dur > 0)) continue;

    const delayMatch = decl.match(/animation-delay\s*:\s*(calc\((?:[^()]+|\([^()]*\))*\)|[+-]?\d*\.?\d+s)/i);
    const delay = delayMatch ? evalCalcSeconds(delayMatch[1], vars) : 0;

    let bezier = [0.42, 0, 0.58, 1];
    const cbMatch = animMatch[3].match(/cubic-bezier\(\s*([^)]+)\s*\)/i);
    if (cbMatch) {
      bezier = cbMatch[1].split(',').map((v) => Number.parseFloat(v.trim()));
    }

    const phase = (((-delay % dur) + dur) % dur) / dur;
    let k0 = kf.stops[0];
    let k1 = kf.stops[kf.stops.length - 1];
    for (let i = 0; i < kf.stops.length - 1; i++) {
      if (phase >= kf.stops[i].p && phase <= kf.stops[i + 1].p) {
        k0 = kf.stops[i];
        k1 = kf.stops[i + 1];
        break;
      }
    }
    const localT = k1.p > k0.p ? (phase - k0.p) / (k1.p - k0.p) : 0;
    const eased = sampleCubicBezier(bezier[0], bezier[1], bezier[2], bezier[3], localT);
    const val = k0.v + (k1.v - k0.v) * eased;
    if (!Number.isFinite(val)) {
      throw new Error(`Computed non-finite keyframe value for #${winId} .${cls}`);
    }

    if (!winClassValues.has(winId)) winClassValues.set(winId, new Map());
    winClassValues.get(winId).set(cls, { propKind: kf.propKind, value: val });
    if (kf.propKind === 'scaleY') matchedSpikeRules++;
  }

  if (matchedSpikeRules < 320) {
    throw new Error(`Expected >= 320 animated spike rules in repo_banner.svg, matched only ${matchedSpikeRules}`);
  }

  let out = svgText.replace(/<style[^>]*>[\s\S]*?<\/style>/i, '');

  for (const [winId, clsMap] of winClassValues.entries()) {
    const winStartToken = `id="${winId}"`;
    const startIdx = out.indexOf(winStartToken);
    if (startIdx === -1) continue;
    const afterStart = out.slice(startIdx + winStartToken.length);
    const nextWinRel = afterStart.search(/id="cspa-win-\d+"|<g transform="translate\(600 0\)"/);
    const endIdx = nextWinRel === -1 ? out.length : startIdx + winStartToken.length + nextWinRel;

    const parentPrefix = out.slice(Math.max(0, startIdx - 160), startIdx);
    const scaleMatch = parentPrefix.match(/scale\(\s*([+-]?\d*\.?\d+)/);
    const parentScale = scaleMatch ? Number.parseFloat(scaleMatch[1]) : 1;

    const vars = winVars.get(winId) ?? { '--spike-gain': 1, '--flip-x': 1, '--flip-y': 1 };
    const ySignGain = (vars['--flip-y'] ?? 1) * (vars['--spike-gain'] ?? 1);
    const flipX = vars['--flip-x'] ?? 1;

    let chunk = out.slice(startIdx, endIdx);

    chunk = chunk.replace(
      /<path\s+class="wf\s+([\w-]+)"\s+d="([^"]+)"(\s+fill="[^"]+")?/g,
      (full, cls, dAttr, fillAttr) => {
        const entry = clsMap.get(cls);
        if (!entry || entry.propKind !== 'scaleY') return full;
        const sy = entry.value * ySignGain;
        if (fillAttr && flipX >= 0) {
          // Filled spike path (no stroke): use SVG scale(1, sy) so userSpaceOnUse vertical fill gradient scales identically to CSS scaleY(sy)
          return `<path class="wf ${cls}" transform="scale(1, ${sy.toFixed(4)})" d="${dAttr}"${fillAttr}`;
        }
        const scaledD = dAttr.replace(/([ML])\s*([+-]?\d*\.?\d+)\s+([+-]?\d*\.?\d+)/g, (_, cmd, xs, ys) => {
          const rawX = Number.parseFloat(xs);
          const rawY = Number.parseFloat(ys);
          const x = flipX < 0 ? 232 - rawX : rawX;
          const y = rawY * sy;
          return `${cmd} ${x.toFixed(2)} ${y.toFixed(2)}`;
        });
        return `<path class="wf ${cls}" d="${scaledD}"${fillAttr ?? ''}`;
      }
    );

    // Compensate stroke-width without vector-effect="non-scaling-stroke" so librsvg preserves local userSpaceOnUse gradients
    chunk = chunk.replace(
      /(stroke="url\(#cspa-win-\d+-(?:spike-rim-grad|hero-rim-grad|center-axis-grad)\)"\s+stroke-width=")([+-]?\d*\.?\d+)(")/g,
      (_, pre, sw, post) => `${pre}${(Number.parseFloat(sw) / parentScale).toFixed(3)}${post}`
    );
    chunk = chunk.replace(/\s+vector-effect="non-scaling-stroke"/g, '');

    for (const [cls, entry] of clsMap.entries()) {
      if (entry.propKind === 'opacity') {
        chunk = chunk.replace(
          new RegExp(`(<g\\b[^>]*class="${cls}"[^>]*)(>)`),
          `$1 opacity="${entry.value.toFixed(3)}"$2`
        );
      }
    }

    out = out.slice(0, startIdx) + chunk + out.slice(endIdx);
  }

  return out;
}

async function ensureBannerPoster() {
  if (!fs.existsSync(bannerSvgPath)) {
    throw new Error(`Missing source banner SVG at ${bannerSvgPath}`);
  }
  if (isUpToDate(posterOutPath, [bannerSvgPath, scriptPath])) {
    return;
  }
  fs.mkdirSync(generatedDir, { recursive: true });
  const rawSvg = fs.readFileSync(bannerSvgPath, 'utf8');
  const bakedSvgBuffer = Buffer.from(bakeInitialCssFrameForLibrsvg(rawSvg), 'utf8');
  await sharp(bakedSvgBuffer, { density: 144 })
    .resize(1440, 480)
    .png({ compressionLevel: 9 })
    .toFile(posterOutPath);
}

function ensureKatexStaticAssets() {
  const katexCssSrc = path.join(katexDistDir, 'katex.min.css');
  const katexFontsSrc = path.join(katexDistDir, 'fonts');
  const katexCssDest = path.join(katexPublicDir, 'katex.min.css');
  const katexFontsDest = path.join(katexPublicDir, 'fonts');

  if (!fs.existsSync(katexCssSrc) || !fs.existsSync(katexFontsSrc)) {
    throw new Error(`Missing KaTeX assets under ${katexDistDir}`);
  }

  if (isUpToDate(katexCssDest, [katexCssSrc, scriptPath]) && fs.existsSync(katexFontsDest)) {
    return;
  }

  fs.mkdirSync(katexFontsDest, { recursive: true });
  fs.copyFileSync(katexCssSrc, katexCssDest);

  for (const entry of fs.readdirSync(katexFontsSrc, { withFileTypes: true })) {
    if (entry.isFile()) {
      fs.copyFileSync(path.join(katexFontsSrc, entry.name), path.join(katexFontsDest, entry.name));
    }
  }
}

await ensureBannerPoster();
ensureKatexStaticAssets();
