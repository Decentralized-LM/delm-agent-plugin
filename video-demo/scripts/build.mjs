import { readFile, writeFile, mkdir, copyFile } from "node:fs/promises";
import { build } from "esbuild";
import { fileURLToPath } from "node:url";
// SF Mono matches the user's native Terminal. Cache locally, never redistribute it.
const fontCache = new URL("../.cache/fonts/", import.meta.url);
await mkdir(fontCache, { recursive: true });
await copyFile(
  "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf",
  new URL("SF-Mono-Regular.otf", fontCache),
);
const read = (path) => readFile(new URL("../" + path, import.meta.url), "utf8");
let html = await read("src/template.html");
const timing = JSON.parse(await read("src/timing.json"));
const workflow = JSON.parse(await read("src/workflow.json"));
html = html.replace('data-duration="40"', `data-duration="${timing.duration}"`);
html = html.replace(/(<video\b[^>]*id="game-footage"[^>]*data-start=")[^"]+/, (_, prefix) => `${prefix}${40 + timing.intro_seconds + timing.codex_extension_seconds}`);
html = html.replace(/(<audio\b[^>]*id="interface-sound"[^>]*data-duration=")[^"]+/, (_, prefix) => `${prefix}${timing.duration}`);
const content = await read("src/content.js");
const prompt = JSON.parse(content.match(/prompt:\s*("(?:[^"\\]|\\.)*")/)[1]);
const installer = JSON.parse(content.match(/installer:\s*("(?:[^"\\]|\\.)*")/)[1]);
function typingField(name, text, start, duration) {
  let seed = 2247;
  const random = () => {
    seed ^= seed << 13;
    seed ^= seed >>> 17;
    seed ^= seed << 5;
    return (seed >>> 0) / 4294967296;
  };
  let burst = 1;
  const weights = [...text].map((character, i) => {
    if (name === "skill") {
      const cadence = .72 + ((i * 29 + 13) % 17) / 22;
      return cadence * (text[i - 1] === " " ? 1.8 : 1);
    }
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

const typing = {fields: [typingField("skill", "$delm:run ", 7.65, .58), typingField("prompt", prompt, 8.25, 3.05), typingField("installer", installer, 1.3, 1.65), typingField("closing", "Faster with DeLM.", 34.9, 1.5)]};
await writeFile(new URL("../src/typing.json", import.meta.url), JSON.stringify(typing, null, 2) + "\n");
html = html.replace('<script src="src/content.js"></script>', `<script>window.DELM_TIMING = ${JSON.stringify(timing)}; window.DELM_TYPING = ${JSON.stringify(typing.fields)}; window.DELM_WORKFLOW = ${JSON.stringify(workflow)};</script>\n<script src="src/content.js"></script>`);
const camera = await build({entryPoints: [fileURLToPath(new URL("../src/terminal-camera.js", import.meta.url))], bundle: true, write: false, format: "iife", minify: true});
html = html.replace('<script src="src/film.js"></script>', `<script>${camera.outputFiles[0].text}</script>\n<script src="src/film.js"></script>`);
const css = (await read("src/film.css"))
  .replaceAll("../assets/", "assets/")
  .replaceAll("../.cache/", ".cache/");
const stylesheet = /<link\s+rel="stylesheet"\s+href="src\/film\.css"\s*\/?\s*>/;
if (!stylesheet.test(html)) throw new Error("The template is missing its stylesheet.");
html = html.replace(stylesheet, `<style>\n${css}\n</style>`);
for (const path of ["src/content.js", "src/features.js", "src/film.js"]) {
  html = html.replace(
    `<script src="${path}"></script>`,
    `<script>\n${await read(path)}\n</script>`,
  );
}
await writeFile(new URL("../index.html", import.meta.url), html);
console.log("Built index.html from src/.");
