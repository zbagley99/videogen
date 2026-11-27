"""Transcribe an audio file with word timestamps; print JSON to stdout.

Usage: uv run --project sidecar/whisper sidecar/whisper/transcribe.py AUDIO [--model NAME] [--language en]

Output: {"language": "en", "duration": 12.3,
         "segments": [{"start": 0.0, "end": 2.1, "text": "...",
                       "words": [{"start": 0.0, "end": 0.3, "word": "Hey"}, ...]}]}
Progress lines ("progress <seconds done> <total>") go to stderr.
"""

import argparse
import json
import os
import sys


def cuda_lib_dirs():
    """The CUDA 12 cuBLAS/cuDNN wheels ship their .so files inside the venv."""
    dirs = []
    try:
        import nvidia.cublas.lib, nvidia.cudnn.lib  # noqa: E401

        for mod in (nvidia.cublas.lib, nvidia.cudnn.lib):
            dirs.append(os.path.dirname(mod.__file__) if mod.__file__ else list(mod.__path__)[0])
    except ImportError:
        pass
    return dirs


def ensure_cuda_libs():
    # The dynamic loader reads LD_LIBRARY_PATH at startup, so re-exec once with it set.
    if os.environ.get("VG_WHISPER_REEXEC"):
        return
    dirs = cuda_lib_dirs()
    if not dirs:
        return
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = ":".join(dirs + [env.get("LD_LIBRARY_PATH", "")]).rstrip(":")
    env["VG_WHISPER_REEXEC"] = "1"
    os.execve(sys.executable, [sys.executable] + sys.argv, env)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audio")
    ap.add_argument("--model", default="large-v3-turbo")
    ap.add_argument("--language", default=None)
    ap.add_argument("--device", default="auto", choices=["auto", "cuda", "cpu"])
    ap.add_argument("--prompt", default="Hey everyone, welcome back! So, today I built something. Did it work? Well... let's find out.")
    args = ap.parse_args()

    ensure_cuda_libs()
    from faster_whisper import WhisperModel

    def load(device):
        compute = "float16" if device == "cuda" else "int8"
        return WhisperModel(args.model, device=device, compute_type=compute)

    if args.device == "auto":
        try:
            model = load("cuda")
            device = "cuda"
        except Exception as e:  # no GPU / libraries: fall back to CPU
            print(f"warning: CUDA unavailable ({e}); using CPU", file=sys.stderr)
            model = load("cpu")
            device = "cpu"
    else:
        device = args.device
        model = load(device)
    print(f"device {device}", file=sys.stderr, flush=True)

    segments, info = model.transcribe(
        args.audio,
        language=args.language,
        word_timestamps=True,
        vad_filter=True,
        condition_on_previous_text=False,
        # A punctuated prompt makes Whisper keep punctuation and casing, which the
        # cue-sheet builder uses to split sentences and pick poses.
        initial_prompt=args.prompt,
    )
    out = {"language": info.language, "duration": info.duration, "segments": []}
    for seg in segments:
        words = [
            {"start": round(w.start, 3), "end": round(w.end, 3), "word": w.word.strip()}
            for w in (seg.words or [])
            if w.word.strip()
        ]
        out["segments"].append({"start": round(seg.start, 3), "end": round(seg.end, 3), "text": seg.text.strip(), "words": words})
        print(f"progress {seg.end:.2f} {info.duration:.2f}", file=sys.stderr, flush=True)
    json.dump(out, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
