"""Synthesize restrained UI foley and mix music using the film's shared timing map."""

import argparse
import array
import hashlib
import json
import math
from pathlib import Path
import random
import shutil
import subprocess
import wave


ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".cache"
AUDIO = ROOT / "assets/audio"
TIMING = json.loads((ROOT / "src/timing.json").read_text())
DURATION = float(TIMING["duration"])
ANCHORS = TIMING["anchors"]
INTRO_SECONDS = float(TIMING["intro_seconds"])
WORKFLOW = json.loads((ROOT / "src/workflow.json").read_text())
MIX_RATE = 48000
FRAMES = round(DURATION * MIX_RATE)
LOUDNESS = "loudnorm=I=-20:TP=-3:LRA=7"
MUSIC_GAIN_DB = -12.87
MUSIC_START_SECONDS = 0
MUSIC_FADE_OUT_SECONDS = 1.5


def map_time(time, reverse=False):
    """Map between original and delivery times using the shared edit points."""
    anchors = [(new + INTRO_SECONDS, old) for old, new in ANCHORS] if reverse else [(old, new + INTRO_SECONDS) for old, new in ANCHORS]
    for (old_start, new_start), (old_end, new_end) in zip(anchors, anchors[1:]):
        if old_start <= time <= old_end:
            return new_start + (time - old_start) * (new_end - new_start) / (old_end - old_start)
    raise ValueError(f"Cue {time}s falls outside the timing map.")


def ffmpeg(*arguments, stats=False):
    return subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "info" if stats else "error",
         "-y", *map(str, arguments)],
        check=True, capture_output=True, text=True,
    )


def loudness_stats(result):
    """Read loudnorm's final JSON block from FFmpeg's diagnostic output."""
    start = result.stderr.rfind("{")
    end = result.stderr.rfind("}")
    if start < 0 or end < start:
        raise ValueError("FFmpeg did not return loudness measurements.")
    return json.loads(result.stderr[start:end + 1])


def probe_music(path):
    if not path.is_file():
        raise ValueError(f"Music file does not exist: {path}")
    result = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0",
         "-show_entries", "format=duration:stream=duration,channels,sample_rate",
         "-of", "json", str(path)],
        check=True, capture_output=True, text=True,
    )
    media = json.loads(result.stdout)
    if not media.get("streams"):
        raise ValueError("Music file has no audio stream.")
    stream = media["streams"][0]
    duration = float(stream.get("duration", media.get("format", {}).get("duration", 0)))
    if not math.isfinite(duration) or duration < DURATION - .5:
        raise ValueError(f"Music must cover the {DURATION:g}-second edit; got {duration:.3f}s.")
    return {"duration": duration, "channels": stream["channels"],
            "sample_rate": int(stream["sample_rate"]), "trimmed_seconds": max(0, duration - DURATION)}


def confirmation_sound():
    """A short two-tone confirmation, with no noise or keycap tail."""
    duration = .055
    samples = []
    for index in range(round(duration * MIX_RATE)):
        t = index / MIX_RATE
        envelope = (1 - math.exp(-t / .0005)) * math.exp(-t / .008)
        envelope *= (1 - t / duration) ** 2
        samples.append(envelope * (math.sin(math.tau * 660 * t)
                                  + .35 * math.sin(math.tau * 1320 * t)))
    peak = max(map(abs, samples))
    return [sample / peak for sample in samples]


def text_tick(seed):
    """A tiny, dry harmonic impulse: digital feedback with no switch or release layer."""
    rng = random.Random(seed)
    duration = .018
    frequency = 1420 * rng.uniform(.985, 1.015)
    samples = []
    for index in range(round(duration * MIX_RATE)):
        t = index / MIX_RATE
        envelope = (1 - math.exp(-t / .0004)) * math.exp(-t / .0022) * (1 - t / duration) ** 2
        phase = math.tau * frequency * t
        samples.append(envelope * (math.sin(phase) + .20 * math.sin(2 * phase) + .04 * math.sin(3 * phase)))
    peak = max(map(abs, samples))
    return [sample / peak for sample in samples]


def create_clicks():
    """Use quiet, dry ticks for input and completed file arrivals."""
    decoded = CACHE / "click-source.wav"
    ffmpeg("-i", AUDIO / "effects.wav", "-ar", MIX_RATE, "-ac", 1, "-c:a", "pcm_s16le", decoded)
    with wave.open(str(decoded), "rb") as source:
        source.setpos(round(3.098 * MIX_RATE))
        raw = array.array("h", source.readframes(round(.014 * MIX_RATE)))
    peak = max(map(abs, raw))
    button = [value / peak for value in raw] + [0] * round(.076 * MIX_RATE)
    button += [.52 * raw[index] / peak for index in range(0, len(raw), 2)]
    output = array.array("f", [0]) * (FRAMES * 2)
    events = []

    def place(time, category, label, samples, gain, pan=0, **detail):
        original_time, time = time, map_time(time)
        events.append({"time": round(time, 6), "original_time": round(original_time, 6),
                       "category": category, "label": label,
                       "duration": round(len(samples) / MIX_RATE, 4), "gain": round(gain, 4), **detail})
        offset = round(time * MIX_RATE) * 2
        if offset + len(samples) * 2 > len(output):
            raise ValueError(f"Cue {label!r} exceeds the film duration.")
        for index, value in enumerate(samples):
            for channel, balance in enumerate((1 - pan, 1 + pan)):
                output[offset + index * 2 + channel] += value * gain * balance

    for time, label in [(28.59, "Play game")]:
        place(time, "mouse_button", label, button, .13, release_offset=.09)
    for time, label in [(3.33, "Run installer"), (12.15, "Submit request")]:
        place(time, "enter_key", label, confirmation_sound(), .09)
    # Picture and foley share exact character-reveal times. Sample them sparsely
    # so word pauses remain audible and the soundtrack never becomes a rattle.
    typing = json.loads((ROOT / "src/typing.json").read_text())["fields"]
    fractions = [.01, .035, .08, .13, .18, .23, .26, .31, .36, .40,
                 .46, .50, .55, .61, .66, .72, .78, .84, .91, .99]
    rng = random.Random(2247)
    for field in typing:
        name, value, times = field["name"], field["text"], field["times"]
        if (len(times) != len(value) or not times
                or any(not math.isfinite(time) for time in times)
                or any(a >= b for a, b in zip(times, times[1:]))):
            raise ValueError(f"Invalid character timing for {name!r}; rebuild the film first.")
        if name == "skill":
            indices = [1, 2, 4, 6, 8]
        elif name == "installer":
            indices = [1, 3, 5, 8, 11, 15, 19]
        elif name == "closing":
            indices = [1, 3, 6, 8, 10, 13, 15, 16]
        else:
            indices = [max(1, round(len(value) * fraction)) for fraction in fractions]
        for index in dict.fromkeys(indices):
            if index > len(value):
                continue
            seed = rng.randrange(100000)
            place(times[index - 1], "typing_key", name,
                  text_tick(seed), rng.uniform(.025, .040), pan=rng.uniform(-.035, .035),
                  character_index=index, character=value[index - 1], seed=seed)
    # The short shell launch is authored directly on the delivery timeline.
    for index in [1, 3, 5]:
        delivery_time = INTRO_SECONDS + 6.58 + .38 * index / len("codex")
        place(map_time(delivery_time, reverse=True), "typing_key", "launch_codex",
              text_tick(400 + index), .030, character_index=index,
              character="codex"[index - 1], seed=400 + index)
    place(map_time(INTRO_SECONDS + 7.28, reverse=True), "enter_key", "Launch Codex",
          confirmation_sound(), .09)
    for seed, share in enumerate(WORKFLOW["shares"]):
        time = share["at"]
        for offset, label in [(WORKFLOW["publish_delay"] + WORKFLOW["publish_duration"], "File published"),
                              (WORKFLOW["import_delay"] + WORKFLOW["import_duration"], "File imported")]:
            place(time + offset, "file_arrival", label, text_tick(300 + seed),
                  .075, pan=.12 if label == "File published" else -.12)
    if max(map(abs, output)) >= 1:
        raise ValueError("UI foley exceeds sample headroom.")
    clicks = AUDIO / "demo-clicks.wav"
    with wave.open(str(clicks), "wb") as destination:
        destination.setparams((2, 2, MIX_RATE, 0, "NONE", "not compressed"))
        destination.writeframes(array.array("h", (round(value * 32767) for value in output)).tobytes())
    return clicks, sorted(events, key=lambda event: event["time"])


def mix_music(path, source_info, clicks, destination):
    # Reuse the approved research-video recording and gain. Do not normalize,
    # stretch, loop, or move its opening; retain the source's natural dynamics.
    source_start = MUSIC_START_SECONDS
    source_end = source_start + DURATION
    prepare = (f"atrim=start={source_start:g}:end={source_end:g},asetpts=PTS-STARTPTS,"
               f"aformat=channel_layouts=stereo,volume={MUSIC_GAIN_DB:g}dB,"
               "afade=t=in:st=0:d=0.015,"
               f"afade=t=out:st={DURATION - MUSIC_FADE_OUT_SECONDS:g}:d={MUSIC_FADE_OUT_SECONDS:g},"
               f"apad,atrim=duration={DURATION:g}")
    filters = (
        "[0:a]aformat=channel_layouts=stereo[clicks];"
        f"[1:a]{prepare}[music];"
        "[clicks][music]amix=inputs=2:duration=first:normalize=0,"
        f"aresample={MIX_RATE},apad,atrim=end_sample={FRAMES}[mix]"
    )
    temporary = CACHE / "soundtrack-mix.wav"
    ffmpeg("-i", clicks, "-i", path, "-filter_complex", filters,
           "-map", "[mix]", "-ar", MIX_RATE, "-ac", 2, "-c:a", "pcm_s16le", temporary)
    final_stats = loudness_stats(ffmpeg(
        "-i", temporary, "-af", f"{LOUDNESS}:print_format=json",
        "-f", "null", "-", stats=True,
    ))
    if float(final_stats["input_tp"]) >= -2:
        raise ValueError("Mixed soundtrack exceeds the reserved 2 dB of headroom.")
    with wave.open(str(temporary), "rb") as output:
        if output.getnframes() != FRAMES:
            raise ValueError(f"Mixed soundtrack is not exactly {DURATION:g} seconds.")
    shutil.copyfile(temporary, destination)
    try:
        source_name = str(path.relative_to(ROOT))
    except ValueError:
        source_name = str(path)
    return {
        "source": source_name,
        "source_start_seconds": source_start,
        "source_end_seconds": source_end,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "input": source_info,
        "music_gain_db": MUSIC_GAIN_DB,
        "click_gain_db": 0,
        "fade_in_seconds": 0.015,
        "fade_out_seconds": MUSIC_FADE_OUT_SECONDS,
        "mix": {"duration": DURATION, "sample_rate": MIX_RATE, "channels": 2,
                "integrated_lufs": float(final_stats["input_i"]),
                "true_peak_dbtp": float(final_stats["input_tp"]),
                "lra_lu": float(final_stats["input_lra"])},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--music", type=Path, help=f"Original music covering {DURATION:g} seconds; longer tracks are trimmed.")
    args = parser.parse_args()
    music_path = args.music.expanduser().resolve() if args.music else None
    source_info = probe_music(music_path) if music_path else None
    CACHE.mkdir(exist_ok=True)
    AUDIO.mkdir(parents=True, exist_ok=True)
    clicks, events = create_clicks()
    soundtrack = AUDIO / "demo-soundtrack.wav"
    music = False
    if music_path:
        music = mix_music(music_path, source_info, clicks, soundtrack)
    else:
        shutil.copyfile(clicks, soundtrack)
    cues = {"duration": DURATION, "timing": TIMING,
            "actions": [e["time"] for e in events if e["category"] in {"mouse_button", "enter_key"}],
            "typing": [e["time"] for e in events if e["category"] == "typing_key"],
            "events": events,
            "sources": {"mouse_button": "effects.wav, 3.098–3.112 seconds, press and quieter release",
                        "typing": "Deterministic dry 18 ms digital harmonic impulses",
                        "enter": "Dry 55 ms two-tone confirmation with no noise",
                        "file_arrival": "Dry 18 ms harmonic ticks at publication and import arrivals"},
            "music": music}
    (AUDIO / "demo-cues.json").write_text(json.dumps(cues, indent=2) + "\n")
    print(f"Wrote {DURATION:g}-second {'music + clicks' if music else 'click-only'} soundtrack: {soundtrack}")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
    except ValueError as error:
        raise SystemExit(str(error)) from error
