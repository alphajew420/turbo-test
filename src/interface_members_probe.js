(() => {
  'use strict';
  // Small interface members: HTMLMediaElement constants and loading, HTMLScriptElement.supports,
  // document.plugins and HTMLCanvasElement.toBlob arity.
  const run = (f) => { try { return ['ok', f()]; } catch (error) { return ['throw', error.name]; } };
  const fn = (f) => (typeof f === 'function' ? { name: f.name, length: f.length } : typeof f);
  const desc = (o, k) => {
    const d = Object.getOwnPropertyDescriptor(o, k);
    if (!d) return null;
    const r = { c: d.configurable, e: d.enumerable };
    if ('value' in d) { r.w = d.writable; r.v = typeof d.value === 'function' ? fn(d.value) : d.value; } else { r.get = fn(d.get); r.set = fn(d.set); }
    return r;
  };
  const media = ['NETWORK_EMPTY', 'NETWORK_IDLE', 'NETWORK_LOADING', 'NETWORK_NO_SOURCE', 'HAVE_NOTHING', 'HAVE_METADATA', 'HAVE_CURRENT_DATA', 'HAVE_FUTURE_DATA', 'HAVE_ENOUGH_DATA'];
  const out = {};
  out.mediaConstants = media.map((k) => [desc(HTMLMediaElement, k), desc(HTMLMediaElement.prototype, k), HTMLVideoElement[k], document.createElement('audio')[k]]);
  out.mediaLoadingDesc = desc(HTMLMediaElement.prototype, 'loading');
  const video = document.createElement('video');
  const steps = [];
  steps.push([video.loading, video.getAttribute('loading')]);
  for (const value of ['lazy', 'LAZY', 'eager', 'bogus', '', null, 7]) {
    video.loading = value;
    steps.push([video.loading, video.getAttribute('loading')]);
  }
  video.setAttribute('loading', ' lazy');
  steps.push([video.loading]);
  video.removeAttribute('loading');
  steps.push([video.loading]);
  out.mediaLoading = steps;
  out.mediaLoadingBrand = run(() => Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'loading').get.call(document.createElement('img')));
  out.scriptSupports = [desc(HTMLScriptElement, 'supports'), ...['classic', 'module', 'importmap', 'speculationrules', 'webbundle', 'text/javascript', 'MODULE', '', 'json'].map((t) => HTMLScriptElement.supports(t)), run(() => HTMLScriptElement.supports())[0], run(() => HTMLScriptElement.supports.call(null, 'module'))];
  out.plugins = [desc(Document.prototype, 'plugins'), document.plugins === document.embeds, Object.prototype.toString.call(document.plugins)];
  out.toBlob = desc(HTMLCanvasElement.prototype, 'toBlob');
  return out;
})()
