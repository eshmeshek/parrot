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
# Scripts the multilingual packages can speak, mapped to the speaker-id prefix
# that handles them. A model whose symbol set is Cyrillic strips everything else
# before synthesis, so an Armenian sentence sent to a Russian speaker arrives
# empty; picking the voice from the script the text is actually written in is
# what makes those languages work at all.
SCRIPT_RANGES = (
    ("hye", ((0x0530, 0x058F),)),  # Armenian
    ("kat", ((0x10A0, 0x10FF), (0x1C90, 0x1CBF))),  # Georgian
)

# Most of the package shares the Cyrillic script, so the language is identified
# by letters that belong to one alphabet and not the others. Letters common to
# several — і, ң, ө, ү, һ, ә — identify nothing and are deliberately absent.
# Order matters: the first match wins, so narrower alphabets come first.
# Russian is the fallback for Cyrillic with no distinctive letter.
CYRILLIC_HINTS = (
    ("bak", "ҙҫ"),  # Bashkir
    ("sah", "ҕ"),  # Yakut
    ("chv", "ӑӗӳ"),  # Chuvash
    ("udm", "ӝӟӥ"),  # Udmurt
    ("tgk", "ҷӣӯҳ"),  # Tajik
    ("tat", "җ"),  # Tatar
    ("kaz", "ұ"),  # Kazakh
    ("ukr", "їєґ"),  # Ukrainian
    ("bel", "ў"),  # Belarusian
)

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

    def voices_for(self, prefix: str) -> list[str]:
        return [v for v in self.voices if v.startswith(f"{prefix}_")]

    def detect_prefix(self, text: str) -> str | None:
        """Speaker prefix for the script the text is written in, if identifiable."""
        counts: dict[str, int] = {}
        cyrillic = 0
        for char in text:
            code = ord(char)
            if 0x0400 <= code <= 0x04FF:
                cyrillic += 1
                continue
            for prefix, ranges in SCRIPT_RANGES:
                if any(low <= code <= high for low, high in ranges):
                    counts[prefix] = counts.get(prefix, 0) + 1
                    break

        if counts:
            best = max(counts, key=counts.get)
            # Only switch away from Cyrillic when the other script actually
            # carries the sentence, not on a stray character.
            if counts[best] >= cyrillic:
                return best

        if cyrillic:
            lowered = text.lower()
            for prefix, distinctive in CYRILLIC_HINTS:
                if any(letter in lowered for letter in distinctive) and self.voices_for(prefix):
                    return prefix
            return "ru"
        return None

    def resolve_voice(self, requested: str | None, text: str = "") -> str:
        if requested and requested in self.voices:
            return requested
        if not self.voices:
            raise RuntimeError("model exposes no voices")

        prefix = self.detect_prefix(text)
        if prefix:
            candidates = self.voices_for(prefix)
            if candidates:
                return candidates[0]

        # The multilingual packages list CIS-language speakers alongside the
        # Russian ones, and the first entry is not necessarily Russian.
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

        voice = self.resolve_voice(request.get("voice"), text)
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

        try:
            audio = self.model.apply_tts(**kwargs)
        except ValueError as exc:
            # The model drops every character outside its own alphabet, so text
            # in a script it cannot read reaches synthesis empty. Say so instead
            # of surfacing the bare ValueError this produces.
            raise ValueError(
                f"voice {voice!r} cannot read this text: it is written in a script "
                "this model has no voice for. Silero covers Cyrillic, Armenian and "
                "Georgian; for anything else switch to the OpenAI engine."
            ) from exc
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
