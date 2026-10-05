import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("../", import.meta.url));
const timing = JSON.parse(
  readFileSync(new URL("../src/timing.json", import.meta.url)),
);
const result = spawnSync(
  "ffprobe",
  [
    "-v",
    "error",
    "-show_entries",
    "format=duration,size:stream=codec_name,width,height,r_frame_rate,codec_type",
    "-of",
    "json",
    "renders/delm-demo.mp4",
  ],
  { cwd: root, encoding: "utf8" },
);
if (result.status !== 0)
  throw new Error(result.stderr || "Render the video first.");
const media = JSON.parse(result.stdout);
const video = media.streams.find((s) => s.codec_type === "video");
const audio = media.streams.find((s) => s.codec_type === "audio");
const duration = Number(media.format.duration);
if (
  video.width !== 3840 ||
  video.height !== 2160 ||
  video.r_frame_rate !== "60/1"
)
  throw new Error("Expected a 3840x2160, 60 fps delivery.");
if (Math.abs(duration - timing.duration) > 0.05)
  throw new Error(`Unexpected duration: ${duration}; expected ${timing.duration}`);
if (!audio) throw new Error("Interface sound is missing.");
const decode = spawnSync(
  "ffmpeg",
  ["-v", "error", "-i", "renders/delm-demo.mp4", "-f", "null", "-"],
  { cwd: root, encoding: "utf8" },
);
if (decode.status !== 0 || decode.stderr.trim())
  throw new Error(decode.stderr || "Decode failed.");
const sound = JSON.parse(
  readFileSync(new URL("../assets/audio/demo-cues.json", import.meta.url)),
);
if (Math.abs(sound.duration - timing.duration) > 0.05)
  throw new Error("Soundtrack cues do not match the current edit duration.");
const checks = {
  duration,
  width: video.width,
  height: video.height,
  fps: 60,
  video: video.codec_name,
  audio: audio.codec_name,
  decoded: true,
  cues: sound.events.length,
  cueCategories: sound.events?.reduce((counts, cue) => {
    counts[cue.category] = (counts[cue.category] || 0) + 1;
    return counts;
  }, {}),
  loudness: sound.mix,
};
mkdirSync(new URL("../.review", import.meta.url), { recursive: true });
writeFileSync(
  new URL("../.review/delivery.json", import.meta.url),
  JSON.stringify(checks, null, 2) + "\n",
);
console.log(checks);
