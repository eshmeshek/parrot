"""Silero TTS sidecar for Parrot.

Silero ships its TTS models only as PyTorch `torch.package` archives — there is no
ONNX export (see snakers4/silero-models#283) — so it cannot be driven by the same
`ort` pipeline that runs Kokoro. This process is the bridge: Parrot spawns it
once, keeps it warm, and talks to it over stdin/stdout.

Wire protocol
-------------
Requests  (stdin) : one JSON object per line, UTF-8.
Responses (stdout): 4-byte little-endian header length, JSON header, raw payload.
Logs      (stderr): plain text, forwarded into Parrot's log.

stdout is used in binary mode and never carries anything but frames.

Commands
--------
{"id": N, "cmd": "ping"}
    -> {"id": N, "ok": true}
{"id": N, "cmd": "voices"}
    -> {"id": N, "ok": true, "voices": [...], "sample_rates": [...]}
{"id": N, "cmd": "synthesize", "text": str, "voice": str,
 "sample_rate": int, "speed": float, "accent": bool}
    -> {"id": N, "ok": true, "sample_rate": int, "samples": int}
       payload: `samples` little-endian float32 values, mono
Any command may fail with {"id": N, "ok": false, "error": str}.
"""

from __future__ import annotations

import json
import os
import struct
import sys
import time
import warnings
from typing import Any

# Silero's packaged module trips a SyntaxWarning on a regex literal at import
# time. It is harmless and would otherwise appear in the log on every start.
warnings.filterwarnings("ignore", category=SyntaxWarning)

import torch  # noqa: E402
from torch.package import PackageImporter  # noqa: E402

DEFAULT_SAMPLE_RATE = 24000
SUPPORTED_SAMPLE_RATES = (8000, 24000, 48000)

# Silero exposes speech rate only through SSML `<prosody rate>`, which takes a
# fixed set of named buckets rather than an arbitrary multiplier, so Parrot's
# continuous speed slider is snapped to the nearest bucket. Anything within
# SPEED_EPSILON of 1.0 is synthesized as plain text, keeping the common case off
# the SSML path entirely.
SPEED_EPSILON = 0.04
PROSODY_BUCKETS = (
    (0.55, "x-slow"),
    (0.80, "slow"),
    (1.00, "medium"),
    (1.35, "fast"),
    (1.70, "x-fast"),
)


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def prosody_rate(speed: float) -> str:
    return min(PROSODY_BUCKETS, key=lambda bucket: abs(bucket[0] - speed))[1]


def escape_ssml(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


class SileroSidecar:
    def __init__(self, model_path: str, threads: int | None) -> None:
        if threads and threads > 0:
            torch.set_num_threads(threads)
        # Inference only: autograd bookkeeping is pure overhead here.
        torch.set_grad_enabled(False)

        started = time.perf_counter()
        importer = PackageImporter(model_path)
        self.model = importer.load_pickle("tts_models", "model")
        self.model.to(torch.device("cpu"))
        self.voices: list[str] = list(getattr(self.model, "speakers", []))
        log(
            f"silero: loaded {os.path.basename(model_path)} in "
            f"{time.perf_counter() - started:.2f}s, {len(self.voices)} voices, "
            f"{torch.get_num_threads()} threads"
        )

    def resolve_voice(self, requested: str | None) -> str:
        if requested and requested in self.voices:
            return requested
        if not self.voices:
            raise RuntimeError("model exposes no voices")
        # Prefer a Russian voice: the multilingual packages list CIS-language
        # speakers alongside the Russian ones, and the first entry is not
        # necessarily Russian.
        for voice in self.voices:
            if voice.startswith("ru_"):
                return voice
        return self.voices[0]

    def synthesize(self, request: dict[str, Any]) -> tuple[int, bytes]:
        text = (request.get("text") or "").strip()
        if not text:
            raise ValueError("empty text")

        sample_rate = int(request.get("sample_rate") or DEFAULT_SAMPLE_RATE)
        if sample_rate not in SUPPORTED_SAMPLE_RATES:
            raise ValueError(
                f"sample_rate {sample_rate} unsupported; "
                f"expected one of {SUPPORTED_SAMPLE_RATES}"
            )

        voice = self.resolve_voice(request.get("voice"))
        accent = bool(request.get("accent", True))
        speed = float(request.get("speed") or 1.0)

        kwargs: dict[str, Any] = {
            "speaker": voice,
            "sample_rate": sample_rate,
            "put_accent": accent,
            "put_yo": accent,
        }
        if abs(speed - 1.0) <= SPEED_EPSILON:
            kwargs["text"] = text
        else:
            rate = prosody_rate(speed)
            kwargs["ssml_text"] = (
                f'<speak><prosody rate="{rate}">{escape_ssml(text)}</prosody></speak>'
            )

        audio = self.model.apply_tts(**kwargs)
        samples = audio.detach().to(torch.float32).contiguous().numpy()
        return sample_rate, samples.tobytes()


def write_frame(header: dict[str, Any], payload: bytes = b"") -> None:
    encoded = json.dumps(header, ensure_ascii=False).encode("utf-8")
    out = sys.stdout.buffer
    out.write(struct.pack("<I", len(encoded)))
    out.write(encoded)
    if payload:
        out.write(payload)
    out.flush()


def main() -> int:
    if len(sys.argv) < 2:
        log("usage: silero_sidecar.py <model.pt> [threads]")
        return 2

    model_path = sys.argv[1]
    threads = int(sys.argv[2]) if len(sys.argv) > 2 else None

    try:
        sidecar = SileroSidecar(model_path, threads)
    except Exception as exc:  # noqa: BLE001 - reported to the parent, then exit
        write_frame({"id": 0, "ok": False, "error": f"model load failed: {exc}"})
        log(f"silero: fatal: {exc}")
        return 1

    # Announce readiness so the parent can tell "still loading" from "hung".
    write_frame({"id": 0, "ok": True, "ready": True, "voices": sidecar.voices})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            write_frame({"id": 0, "ok": False, "error": f"malformed request: {exc}"})
            continue

        request_id = request.get("id", 0)
        command = request.get("cmd")
        try:
            if command == "ping":
                write_frame({"id": request_id, "ok": True})
            elif command == "voices":
                write_frame(
                    {
                        "id": request_id,
                        "ok": True,
                        "voices": sidecar.voices,
                        "sample_rates": list(SUPPORTED_SAMPLE_RATES),
                    }
                )
            elif command == "synthesize":
                started = time.perf_counter()
                sample_rate, payload = sidecar.synthesize(request)
                count = len(payload) // 4
                log(
                    f"silero: synth {count / sample_rate:.2f}s audio in "
                    f"{time.perf_counter() - started:.2f}s"
                )
                write_frame(
                    {
                        "id": request_id,
                        "ok": True,
                        "sample_rate": sample_rate,
                        "samples": count,
                    },
                    payload,
                )
            elif command == "shutdown":
                write_frame({"id": request_id, "ok": True})
                return 0
            else:
                write_frame(
                    {"id": request_id, "ok": False, "error": f"unknown cmd {command!r}"}
                )
        except Exception as exc:  # noqa: BLE001 - a bad request must not kill the process
            write_frame({"id": request_id, "ok": False, "error": str(exc)})
            log(f"silero: request {request_id} failed: {exc}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
