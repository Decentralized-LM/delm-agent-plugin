import { readFile, writeFile, mkdir, copyFile } from "node:fs/promises";
// SF Mono matches the user's native Terminal. Cache locally, never redistribute it.
const fontCache = new URL("../.cache/fonts/", import.meta.url);
await mkdir(fontCache, { recursive: true });
for (const face of ["SF-Mono-Regular.otf", "SF-Mono-Bold.otf"]) {
  await copyFile(`/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/${face}`, new URL(face, fontCache));
}
const read = (path) => readFile(new URL("../" + path, import.meta.url), "utf8");
let html = await read("src/template.html");
const timing = JSON.parse(await read("src/timing.json"));
html = html.replace('data-duration="40"', `data-duration="${timing.duration}"`);
html = html.replace(/(<video\b[^>]*id="game-footage"[^>]*data-start=")[^"]+/, (_, prefix) => `${prefix}${timing.result.footage}`);
html = html.replace(/(<audio\b[^>]*id="interface-sound"[^>]*data-duration=")[^"]+/, (_, prefix) => `${prefix}${timing.duration}`);
const content = await read("src/content.js");
const field = (name) => JSON.parse(content.match(new RegExp(`${name}:\\s*("(?:[^"\\\\]|\\\\.)*")`))[1]);
const prompt = field("prompt"), installer = field("installer");
function typingField(name, text, [start, duration]) {
  let seed = 2247;
  const random = () => {
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return (seed >>> 0) / 4294967296;
  };
  let burst = 1;
  const weights = [...text].map((character, i) => {
    let pause = 1;
    if (i === 0 || text[i - 1] === " ") {
      // Some words flow together; others leave a brief planning pause.
      burst = .80 + .38 * random();
      const boundary = random();
      pause = boundary < .18 ? 2.1 + .8 * random()
        : boundary < .42 ? 1.2 + .35 * random() : .88 + .22 * random();
      if (/[,.!?] $/.test(text.slice(0, i))) pause = 2.4 + .9 * random();
    }
    return (.78 + .44 * random()) * burst * pause;
  });
  const total = weights.reduce((a, b) => a + b, 0);
  let elapsed = 0;
  const times = weights.map(weight => start + duration * ((elapsed += weight) / total));
  times[times.length - 1] = start + duration;
  return {name, text, times};
}

const typing = {fields: [
  typingField("installer", installer, timing.install.typing),
  typingField("choice", "3", timing.install.choice),
  typingField("launch", "claude", timing.install.launch),
  typingField("slash", "/delm:", timing.claude.slash),
  typingField("prompt", prompt, timing.claude.prompt),
  typingField("closing", "Faster with DeLM.", timing.closing.typing),
]};
await writeFile(new URL("../src/typing.json", import.meta.url), JSON.stringify(typing, null, 2) + "\n");
html = html.replace('<script src="src/content.js"></script>', () => `<script>window.DELM_TIMING = ${JSON.stringify(timing)}; window.DELM_TYPING = ${JSON.stringify(typing.fields)};</script>\n<script src="src/content.js"></script>`);
// Inline the vector mascots so their arms and eyes can move.
for (const name of ["clawd", "codex"]) {
  const svg = (await read(`assets/pets/${name}.svg`)).trim();
  html = html.replaceAll(`<!--mascot:${name}-->`, () => svg);
}
const css = (await read("src/film.css"))
  .replaceAll("../assets/", "assets/")
  .replaceAll("../.cache/", ".cache/");
const stylesheet = /<link\s+rel="stylesheet"\s+href="src\/film\.css"\s*\/?\s*>/;
if (!stylesheet.test(html)) throw new Error("The template is missing its stylesheet.");
html = html.replace(stylesheet, () => `<style>\n${css}\n</style>`);
for (const path of ["src/content.js", "src/features.js", "src/claude.js", "src/film.js"]) {
  const script = await read(path);
  // A replacer function keeps "$$" and "$&" in the source literal.
  html = html.replace(`<script src="${path}"></script>`, () => `<script>\n${script}\n</script>`);
}
await writeFile(new URL("../index.html", import.meta.url), html);
console.log("Built index.html from src/.");
