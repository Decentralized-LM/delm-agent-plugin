/* Minimal browser boundary for controller integration tests. The actual game,
 * renderer and simulation run unchanged; this is not a visual browser test. */
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');

class Target {
  constructor() { this.listeners = new Map(); }
  addEventListener(type, handler) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(handler);
  }
  removeEventListener(type, handler) {
    this.listeners.set(type, (this.listeners.get(type) || []).filter(item => item !== handler));
  }
  async fire(type, supplied = {}) {
    const event = { type, target: this, currentTarget: this, preventDefault() { this.defaultPrevented = true; },
      stopPropagation() {}, ...supplied };
    for (const handler of this.listeners.get(type) || []) await handler(event);
    return event;
  }
}
class Classes {
  constructor(value = '') { this.values = new Set(value.split(/\s+/).filter(Boolean)); }
  add(...values) { values.forEach(value => this.values.add(value)); }
  remove(...values) { values.forEach(value => this.values.delete(value)); }
  contains(value) { return this.values.has(value); }
  toggle(value, force) {
    const enabled = force === undefined ? !this.values.has(value) : !!force;
    if (enabled) this.values.add(value); else this.values.delete(value);
    return enabled;
  }
}
function canvasContext() {
  const gradient = { addColorStop() {} };
  const methods = ['setTransform', 'resetTransform', 'save', 'restore', 'scale', 'rotate', 'translate',
    'beginPath', 'closePath', 'moveTo', 'lineTo', 'bezierCurveTo', 'quadraticCurveTo',
    'arc', 'arcTo', 'ellipse', 'rect', 'roundRect', 'fill', 'stroke', 'clip',
    'clearRect', 'fillRect', 'strokeRect', 'fillText', 'strokeText', 'drawImage', 'setLineDash'];
  const context = {};
  methods.forEach(name => { context[name] = () => {}; });
  context.createLinearGradient = context.createRadialGradient = () => gradient;
  context.measureText = text => ({ width: String(text).length * 7 });
  return context;
}
class Element extends Target {
  constructor(id, tag = 'div', attributes = '') {
    super(); this.id = id; this.tagName = tag.toUpperCase(); this.attributes = {};
    this.style = { setProperty(name, value) { this[name] = value; } };
    this.classList = new Classes(attributes.match(/\bclass="([^"]*)"/)?.[1]);
    this.hidden = /(?:^|\s)hidden(?:\s|$|=)/.test(attributes);
    this.value = attributes.match(/\bvalue="([^"]*)"/)?.[1] || '';
    this.checked = /(?:^|\s)checked(?:\s|$|=)/.test(attributes);
    this.textContent = ''; this.innerHTML = ''; this.dataset = {};
    this.clientWidth = 1200; this.clientHeight = 760;
    this.width = 1200; this.height = 760;
    this._context = canvasContext();
  }
  getContext() { return this._context; }
  getBoundingClientRect() { return { x: 0, y: 0, top: 0, left: 0, right: this.clientWidth,
    bottom: this.clientHeight, width: this.clientWidth, height: this.clientHeight }; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] || null; }
  removeAttribute(name) { delete this.attributes[name]; }
  focus() { if (this.ownerDocument) this.ownerDocument.activeElement = this; }
  blur() { if (this.ownerDocument) this.ownerDocument.activeElement = this.ownerDocument.body; }
  setPointerCapture() {}
  releasePointerCapture() {}
  hasPointerCapture() { return true; }
  closest(selector) {
    return selector.split(',').some(tag => tag.trim().toUpperCase() === this.tagName) ? this : null;
  }
}

function createDOM(options = {}) {
  const directory = path.resolve(__dirname, '../..');
  const html = fs.readFileSync(path.join(directory, 'index.html'), 'utf8');
  const document = new Target();
  const elements = new Map();
  const allElements = [];
  for (const match of html.matchAll(/<([\w-]+)\b([^>]*)>/g)) {
    const id = match[2].match(/\bid="([^"]+)"/)?.[1] || '';
    const element = new Element(id, match[1], match[2]);
    allElements.push(element);
    if (id) elements.set(id, element);
  }
  document.documentElement = new Element('html', 'html');
  document.body = new Element('body', 'body');
  document.activeElement = document.body;
  document.hidden = false;
  document.visibilityState = 'visible';
  document.getElementById = id => elements.get(id) || null;
  document.querySelectorAll = selector => allElements.filter(element => selector.split(',').some(value => {
    const item = value.trim();
    return item.startsWith('#') ? element.id === item.slice(1) : item.startsWith('.')
      ? element.classList.contains(item.slice(1)) : element.tagName.toLowerCase() === item;
  }));
  document.querySelector = selector => document.querySelectorAll(selector)[0] || null;
  document.createElement = tag => new Element('', tag);
  allElements.forEach(element => {
    element.ownerDocument = document;
    // Only focus trapping requires subtree selection; these overlays have
    // direct, uniquely named controls in the source document.
    element.querySelectorAll = selector => {
      const opening = html.indexOf(`id="${element.id}"`);
      const closing = html.indexOf('</section>', opening);
      const fragment = html.slice(opening, closing);
      return document.querySelectorAll(selector).filter(child => child.id && fragment.includes(`id="${child.id}"`));
    };
  });
  const window = new Target();
  const storage = new Map(Object.entries(options.storage || {}));
  let now = 0;
  let nextFrame = null;
  const timers = new Map();
  let timerId = 0;
  const sandbox = {
    console, document, navigator: { maxTouchPoints: 0, vibrate() {} },
    innerWidth: 1200, innerHeight: 760, devicePixelRatio: 1,
    performance: { now: () => now },
    localStorage: {
      getItem(key) { if (options.storageDenied) throw new Error('Storage denied'); return storage.get(key) ?? null; },
      setItem(key, value) { if (options.storageDenied) throw new Error('Storage denied'); storage.set(key, String(value)); }
    },
    matchMedia: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }),
    requestAnimationFrame: callback => { nextFrame = callback; return 1; },
    cancelAnimationFrame: () => { nextFrame = null; },
    setTimeout: (callback, delay = 0) => { const id = ++timerId; timers.set(id, { callback, at: now + delay }); return id; },
    clearTimeout: id => timers.delete(id),
    addEventListener: window.addEventListener.bind(window),
    removeEventListener: window.removeEventListener.bind(window),
    ResizeObserver: class { constructor(callback) { this.callback = callback; } observe() {} disconnect() {} },
    Image: class {},
    ...options.globals
  };
  sandbox.window = sandbox; sandbox.globalThis = sandbox;
  const context = vm.createContext(sandbox);
  const scripts = [...html.matchAll(/<script\b[^>]*\bsrc="([^"]+)"[^>]*>/g)].map(match => match[1]);
  for (const filename of scripts) {
    if (options.beforeScript) options.beforeScript(filename, context);
    vm.runInContext(fs.readFileSync(path.join(directory, filename), 'utf8'), context, { filename });
  }
  function frame(milliseconds = 1000 / 60) {
    now += milliseconds;
    for (const [id, timer] of timers) {
      if (timer.at <= now) { timers.delete(id); timer.callback(); }
    }
    const callback = nextFrame; nextFrame = null;
    if (callback) callback(now);
  }
  return { context, document, window, elements, storage, frame,
    step(seconds, fps = 60) { for (let i = 0; i < Math.round(seconds * fps); i++) frame(1000 / fps); },
    element: id => { const element = elements.get(id); if (!element) throw new Error(`Missing element #${id}`); return element; },
    evaluate: source => vm.runInContext(source, context)
  };
}

module.exports = { createDOM };
