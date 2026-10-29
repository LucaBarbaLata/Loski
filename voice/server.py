"""Loski's graphics-card voice (talking back, beta): Chatterbox on an NVIDIA
card, saying what Loski sends it.

It only listens on Loski's own Docker network. POST /say with {"text": ...}
returns a 16-bit mono WAV; GET /health says whether the model is loaded
(about a minute and a half after starting, longer the first time, while
the 3 GB model downloads).
"""

import io
import json
import os
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import torch
from chatterbox.tts import ChatterboxTTS

PORT = int(os.environ.get("PORT", "8800"))
# A little calmer than Chatterbox's default, which can get dramatic.
EXAGGERATION = float(os.environ.get("EXAGGERATION", "0.35"))
CFG_WEIGHT = float(os.environ.get("CFG_WEIGHT", "0.5"))
# Loski's lines are short; anything longer is cut.
MAX_TEXT = 300

model = None
ready = threading.Event()
# One line at a time on the graphics card.
busy = threading.Lock()


def load():
    global model
    started = time.time()
    model = ChatterboxTTS.from_pretrained(device="cuda")
    with busy, torch.inference_mode():
        model.generate("Ready.", exaggeration=EXAGGERATION, cfg_weight=CFG_WEIGHT)
    ready.set()
    print(f"Loski's voice is ready ({time.time() - started:.0f} s)", flush=True)


def wav_bytes(samples, rate):
    pcm = (np.clip(samples, -1.0, 1.0) * 32767).astype("<i2")
    out = io.BytesIO()
    with wave.open(out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())
    return out.getvalue()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/health":
            return self.answer(404, b"{}")
        state = b'{"ready": true}' if ready.is_set() else b'{"ready": false}'
        self.answer(200 if ready.is_set() else 503, state)

    def do_POST(self):
        if self.path != "/say":
            return self.answer(404, b"{}")
        if not ready.is_set():
            return self.answer(503, b'{"ready": false}')
        try:
            length = min(int(self.headers.get("Content-Length", "0")), 4096)
            text = str(json.loads(self.rfile.read(length)).get("text", "")).strip()[:MAX_TEXT]
        except (ValueError, json.JSONDecodeError):
            return self.answer(400, b'{"error": "bad request"}')
        if not text:
            return self.answer(400, b'{"error": "nothing to say"}')
        with busy, torch.inference_mode():
            spoken = model.generate(text, exaggeration=EXAGGERATION, cfg_weight=CFG_WEIGHT)
        self.answer(200, wav_bytes(spoken.squeeze(0).cpu().numpy(), model.sr), "audio/wav")

    def answer(self, status, body, kind="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    threading.Thread(target=load, daemon=True).start()
    print(f"Loski's voice is loading, on port {PORT}", flush=True)
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
