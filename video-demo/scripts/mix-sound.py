"""Synthesize the interface sound design and mix it with the music on the film's timing map."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

import numpy as np
from scipy import signal
from scipy.io import wavfile


ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".cache"
AUDIO = ROOT / "assets/audio"
TIMING = json.loads((ROOT / "src/timing.json").read_text())
DURATION = float(TIMING["duration"])
RATE = 48000
FRAMES = round(DURATION * RATE)
MUSIC_GAIN_DB = -12.87
MUSIC_FADE_OUT_SECONDS = 1.5
TARGET_LUFS = -16.0
TRUE_PEAK = -1.5


def seconds(duration):
    return np.arange(round(duration * RATE)) / RATE


def bandpass(samples, low, high, order=2):
    sos = signal.butter(order, [low, high], btype="band", fs=RATE, output="sos")
    return signal.sosfilt(sos, samples)


def lowpass(samples, cutoff, order=2):
    return signal.sosfilt(signal.butter(order, cutoff, fs=RATE, output="sos"), samples)


def normalized(samples):
    peak = np.max(np.abs(samples))
    return samples / peak if peak else samples


def key(rng, weight=1.0):
    """A keyboard stroke: switch click, keycap body, and bottom-out."""
    t = seconds(.07)
    click = bandpass(rng.standard_normal(t.size), 2400, 7800) * np.exp(-t / .0022)
    body = np.sin(2 * np.pi * rng.uniform(190, 255) * t) * np.exp(-t / .016) * .55
    tick = np.sin(2 * np.pi * rng.uniform(3000, 3600) * t) * np.exp(-t / .0035) * .22
    bottom = np.zeros_like(t)
    start = round(rng.uniform(.009, .014) * RATE)
    tail = t[: t.size - start]
    bottom[start:] = lowpass(rng.standard_normal(tail.size), 1500) * np.exp(-tail / .008) * .5
    return normalized(click + body + tick + bottom) * weight


def enter(rng):
    """A larger key with a deeper body and a second contact."""
    t = seconds(.12)
    click = bandpass(rng.standard_normal(t.size), 1800, 6500) * np.exp(-t / .003)
    body = np.sin(2 * np.pi * 150 * t) * np.exp(-t / .03) * .9
    second = np.zeros_like(t)
    start = round(.022 * RATE)
    tail = t[: t.size - start]
    second[start:] = lowpass(rng.standard_normal(tail.size), 1200) * np.exp(-tail / .012) * .6
    return normalized(click + body + second)


def pluck(frequency, decay=.45, brightness=.35):
    """A soft bell pluck with two slightly detuned voices."""
    t = seconds(decay * 4)
    attack = 1 - np.exp(-t / .0025)
    tone = np.zeros_like(t)
    for detune in (-.0018, .0018):
        f = frequency * (1 + detune)
        tone += np.sin(2 * np.pi * f * t) * np.exp(-t / decay)
        tone += brightness * np.sin(2 * np.pi * 2 * f * t) * np.exp(-t / (decay * .45))
        tone += .12 * np.sin(2 * np.pi * 3.01 * f * t) * np.exp(-t / (decay * .25))
    return normalized(tone * attack)


def chime(frequencies, spacing=.075, decay=.55):
    voices = [pluck(f, decay) for f in frequencies]
    length = max(round(i * spacing * RATE) + v.size for i, v in enumerate(voices))
    out = np.zeros(length)
    for index, voice in enumerate(voices):
        start = round(index * spacing * RATE)
        out[start:start + voice.size] += voice * (1 - .12 * index)
    return normalized(out)


def whoosh(rng, duration, low, high, rise=.55):
    """Filtered air that sweeps from low to high and settles."""
    t = seconds(duration)
    noise = rng.standard_normal(t.size)
    bands = [bandpass(noise, f * .7, f * 1.4) for f in np.geomspace(low, high, 6)]
    out = np.zeros_like(t)
    position = t / duration
    for index, band in enumerate(bands):
        center = index / (len(bands) - 1)
        out += band * np.exp(-((position - center * rise - .1) / .18) ** 2)
    envelope = np.sin(np.pi * np.clip(position, 0, 1)) ** 1.6
    return normalized(out * envelope)


def pop(start_frequency, end_frequency, duration=.11):
    t = seconds(duration)
    frequency = start_frequency * (end_frequency / start_frequency) ** (t / duration)
    phase = 2 * np.pi * np.cumsum(frequency) / RATE
    envelope = (1 - np.exp(-t / .004)) * np.exp(-t / (duration * .4))
    return normalized(np.sin(phase) * envelope + .25 * np.sin(2 * phase) * envelope)


def ratchet(rng):
    t = seconds(.02)
    return normalized(bandpass(rng.standard_normal(t.size), 2200, 5200) * np.exp(-t / .002)
                      + .4 * np.sin(2 * np.pi * 1700 * t) * np.exp(-t / .004))


def error_tone():
    return chime([622.25, 466.16], spacing=.11, decay=.22)


def mouse_click():
    """The research video's click: a short press with a quieter release."""
    decoded = CACHE / "click-source.wav"
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", AUDIO / "effects.wav",
                    "-ar", str(RATE), "-ac", "1", decoded], check=True)
    rate, data = wavfile.read(decoded)
    data = data.astype(np.float64) / 32768
    press = data[round(3.098 * rate):round(3.112 * rate)]
    gap = np.zeros(round(.076 * RATE))
    return normalized(np.concatenate([press, gap, press[::2] * .52]))


class Bus:
    def __init__(self):
        self.dry = np.zeros((FRAMES, 2))
        self.send = np.zeros((FRAMES, 2))
        self.events = []

    def place(self, time, category, label, samples, gain, pan=0.0, reverb=.12):
        start = round(time * RATE)
        if start < 0 or start + samples.size > FRAMES:
            raise ValueError(f"Cue {label!r} at {time:.2f}s falls outside the film.")
        left, right = np.cos((pan + 1) * np.pi / 4), np.sin((pan + 1) * np.pi / 4)
        stereo = np.outer(samples * gain, [left * np.sqrt(2), right * np.sqrt(2)])
        self.dry[start:start + samples.size] += stereo
        self.send[start:start + samples.size] += stereo * reverb
        self.events.append({"time": round(time, 4), "category": category, "label": label,
                            "gain": round(gain, 4), "pan": round(pan, 3)})


def room(rng):
    """A short, bright room: decorrelated stereo tails with early reflections."""
    t = seconds(.75)
    channels = []
    for _ in range(2):
        tail = lowpass(rng.standard_normal(t.size), 6500) * np.exp(-t / .16)
        tail[: round(.012 * RATE)] = 0
        channels.append(tail / np.sqrt(np.sum(tail ** 2)))
    return np.stack(channels, axis=1)


def build_effects():
    rng = np.random.default_rng(2247)
    bus = Bus()
    typing = {field["name"]: field for field in json.loads((ROOT / "src/typing.json").read_text())["fields"]}
    title, install, claude, board = TIMING["title"], TIMING["install"], TIMING["claude"], TIMING["board"]
    agents, result, closing = TIMING["agents"], TIMING["result"], TIMING["closing"]

    # Title: a hello, a wheel turning, and a second hello.
    bus.place(title["clawd_wave"], "mascot", "Clawd waves", pop(520, 880), .16, -.15, .2)
    for index in range(7):
        p = index / 6
        at = title["roll"] + title["roll_duration"] * (1 - np.cos(np.pi * p)) / 2
        bus.place(at, "wheel", "Title wheel", ratchet(rng), .06 * (1 - .5 * abs(p - .5)), .0, .1)
    bus.place(title["codex_wave"], "mascot", "Codex waves", pop(600, 980), .16, .15, .2)
    bus.place(install["window_in"] - .1, "air", "Terminal rises", whoosh(rng, .7, 300, 2400), .07, 0, .25)

    # Typing: every stroke of the shell and closing lines; the long prompt at a natural pace.
    for name, field in typing.items():
        step = 2 if name == "prompt" else 1
        for index, at in enumerate(field["times"]):
            if index % step or field["text"][index] == " " and name != "prompt":
                continue
            weight = rng.uniform(.82, 1.0)
            gain = .075 if name == "closing" else .062
            bus.place(at, "typing_key", name, key(rng, weight), gain, rng.uniform(-.08, .08), .07)
    for at, label in [(install["enter"], "Run installer"), (install["choice_enter"], "Choose both"),
                      (install["launch_enter"], "Launch Claude Code"), (claude["submit"], "Send request")]:
        bus.place(at, "enter_key", label, enter(rng), .2, 0, .1)
    bus.place(claude["complete"], "typing_key", "Complete command", key(rng), .07, 0, .07)
    bus.place(install["claude"][1], "success", "Installed in Claude Code", pluck(1318.5, .35), .1, -.1, .2)
    bus.place(install["codex"][1], "success", "Installed in Codex", pluck(1568.0, .35), .1, .1, .2)
    bus.place(install["ready"], "success", "Ready in both", chime([1318.5, 1975.5], .08, .5), .11, 0, .22)
    bus.place(claude["expand"] - .05, "air", "Window grows", whoosh(rng, .9, 160, 1400), .07, 0, .25)

    # The board and the agents.
    bus.place(board["open"], "board", "Board opens", whoosh(rng, .5, 600, 3200, .7), .08, .35, .2)
    bus.place(board["open"] + .05, "board", "Board opens", pluck(987.8, .3), .1, .35, .2)
    bus.place(agents["in"], "board", "Agent 1 appears", pop(440, 660, .09), .13, -.35, .2)
    bus.place(agents["in"] + .14, "board", "Agent 2 appears", pop(494, 740, .09), .13, .1, .2)
    pans = {1: -.4, 2: .05}
    notes = {"claim": [783.99], "share": [1318.5, 1975.5], "import": [1975.5, 1318.5, 2637.0]}
    durations = TIMING["route_duration"]
    for route in TIMING["routes"]:
        end = route["at"] + durations[route["kind"]]
        pan = pans[route["agent"]]
        bus.place(route["at"], "route", f"{route['kind']} departs", whoosh(rng, .32, 900, 4200, .8), .045, pan, .15)
        bus.place(end, "route", f"{route['kind']} arrives", chime(notes[route["kind"]], .07, .45), .135, .4, .22)
    for agent in ("1", "2"):
        for beat in agents[agent]:
            if not beat.get("type"):
                continue
            characters = sum(len(line) for line in beat["code"])
            for index in range(0, characters, 3):
                at = beat["at"] + beat["type"] * index / characters
                bus.place(at, "agent_typing", f"Agent {agent} edits", key(rng, rng.uniform(.6, .9)), .03, pans[int(agent)], .05)
            if beat.get("tone") == "bad":
                bus.place(beat["at"], "error", "Bug found", error_tone(), .08, pans[int(agent)], .2)
    bus.place(agents["1"][-1]["at"], "success", "Tests pass", chime([1046.5, 1318.5, 1568.0], .06, .5), .11, -.3, .22)
    bus.place(board["delivered"], "success", "Changes applied", chime([1046.5, 1318.5, 1568.0, 2093.0], .09, .7), .13, .2, .25)

    # Result and close.
    bus.place(result["zoom"][0], "air", "Push into the message", whoosh(rng, .95, 200, 3200, .85), .1, 0, .2)
    bus.place(result["game"], "result", "Game opens", chime([523.25, 783.99, 1046.5], .05, .8), .1, 0, .3)
    bus.place(result["play_click"], "mouse_button", "Play game", mouse_click(), .2, -.2, .05)
    for at in result["jumps"]:
        bus.place(at, "game", "Jump", pop(420, 980, .12), .13, -.3, .15)
    for index, at in enumerate(result["callouts"]):
        bus.place(at, "result", f"Agent {index + 1} callout", pluck(1318.5 if index == 0 else 1568.0, .35), .09, -.3 if index == 0 else .4, .22)
    bus.place(result["exit"], "air", "Game closes", whoosh(rng, .7, 1800, 300, .7), .05, 0, .2)
    bus.place(closing["command"] + .05, "closing", "Command appears", pluck(1174.7, .4), .08, 0, .2)
    bus.place(closing["mascots"], "mascot", "Clawd hops", pop(380, 820, .14), .16, -.1, .22)
    bus.place(closing["mascots"] + .16, "mascot", "Codex hops", pop(440, 940, .14), .16, .1, .22)

    impulse = room(rng)
    reverb = np.stack([signal.fftconvolve(bus.send[:, c], impulse[:, c])[:FRAMES] for c in range(2)], axis=1)
    effects = bus.dry + reverb * .9
    return effects, sorted(bus.events, key=lambda event: event["time"])


def ffmpeg(*arguments):
    return subprocess.run(["ffmpeg", "-hide_banner", "-y", *map(str, arguments)],
                          check=True, capture_output=True, text=True)


def music(path):
    decoded = CACHE / "music.wav"
    ffmpeg("-loglevel", "error", "-i", path, "-t", f"{DURATION:g}", "-ar", RATE, "-ac", 2,
           "-af", f"volume={MUSIC_GAIN_DB}dB,afade=t=in:st=0:d=0.015,"
                  f"afade=t=out:st={DURATION - MUSIC_FADE_OUT_SECONDS:g}:d={MUSIC_FADE_OUT_SECONDS:g}",
           "-c:a", "pcm_f32le", decoded)
    _, data = wavfile.read(decoded)
    data = data.astype(np.float64)
    out = np.zeros((FRAMES, 2))
    out[: min(FRAMES, len(data))] = data[:FRAMES]
    return out


def loudness(path):
    result = ffmpeg("-i", path, "-af", "loudnorm=print_format=json", "-f", "null", "-")
    text = result.stderr
    return json.loads(text[text.rfind("{"):text.rfind("}") + 1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--music", type=Path, required=True)
    args = parser.parse_args()
    CACHE.mkdir(exist_ok=True)
    effects, events = build_effects()
    wavfile.write(AUDIO / "demo-clicks.wav", RATE, (np.clip(effects, -1, 1) * 32767).astype(np.int16))
    mix = music(args.music.expanduser().resolve()) + effects
    raw = CACHE / "soundtrack-raw.wav"
    wavfile.write(raw, RATE, mix.astype(np.float32))
    # Normalize the whole mix to web loudness, then hold true peaks below the ceiling.
    measured = loudness(raw)
    gain = TARGET_LUFS - float(measured["input_i"])
    soundtrack = AUDIO / "demo-soundtrack.wav"
    ffmpeg("-loglevel", "error", "-i", raw, "-af",
           f"volume={gain:.3f}dB,alimiter=limit={10 ** (TRUE_PEAK / 20):.4f}:attack=2:release=60:level=disabled",
           "-ar", RATE, "-c:a", "pcm_s24le", soundtrack)
    final = loudness(soundtrack)
    cues = {
        "duration": DURATION,
        "events": events,
        "music": {"source": str(args.music), "sha256": hashlib.sha256(args.music.read_bytes()).hexdigest(),
                  "gain_db": MUSIC_GAIN_DB, "fade_out_seconds": MUSIC_FADE_OUT_SECONDS},
        "mix": {"integrated_lufs": float(final["input_i"]), "true_peak_dbtp": float(final["input_tp"]),
                "lra_lu": float(final["input_lra"]), "normalization_gain_db": round(gain, 2)},
    }
    (AUDIO / "demo-cues.json").write_text(json.dumps(cues, indent=2) + "\n")
    print(f"Wrote {DURATION:g}-second soundtrack at {final['input_i']} LUFS, "
          f"{final['input_tp']} dBTP, {len(events)} cues: {soundtrack.relative_to(ROOT)}")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
    except ValueError as error:
        raise SystemExit(str(error)) from error
