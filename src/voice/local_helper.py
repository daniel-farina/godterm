"""GodTerm local voice helper: keeps one mlx-audio model warm and speaks
sentences for GodTerm over stdin/stdout (a private pipe, nothing listens
on the network).

GodTerm writes one JSON request per line on stdin:
  {"op": "speak", "id": 7, "text": "...", "voice": "US_Emma", "instruct": "", "ref": ""}
  {"op": "cancel", "id": 7}
  {"op": "ping"} / {"op": "quit"}
and reads one JSON header per line on stdout, each "audio" header followed
by `bytes` bytes of little endian float32 mono samples:
  {"ev": "ready", "rate": 24000, "load_ms": 2400}
  {"ev": "audio", "id": 7, "rate": 24000, "bytes": 96000}
  {"ev": "done", "id": 7, "cancelled": false}
  {"ev": "error", "id": 7, "msg": "..."}
Everything else (logs, warnings) goes to stderr. The helper exits when
stdin closes, so it never outlives GodTerm.

Run as: python local_helper.py --engine chatterbox|ming --model REPO_OR_DIR
"""

import argparse
import json
import os
import queue
import sys
import threading
import time

PROTO = sys.stdout.buffer
sys.stdout = sys.stderr
os.environ.setdefault("HF_HUB_OFFLINE", "1")

LOCK = threading.Lock()


def send(head, pcm=None):
    data = b""
    if pcm is not None:
        data = pcm.astype("<f4").tobytes()
        head = dict(head, bytes=len(data))
    with LOCK:
        PROTO.write((json.dumps(head) + "\n").encode())
        if data:
            PROTO.write(data)
        PROTO.flush()


def hub_cache():
    if os.environ.get("HF_HUB_CACHE"):
        return os.environ["HF_HUB_CACHE"]
    home = os.environ.get("HF_HOME") or os.path.expanduser("~/.cache/huggingface")
    return os.path.join(home, "hub")


def model_dir(repo):
    """The local snapshot of `repo` (never a download)."""
    if os.path.isdir(repo):
        return repo
    d = os.path.join(hub_cache(), "models--" + repo.replace("/", "--"))
    try:
        with open(os.path.join(d, "refs", "main")) as f:
            snap = os.path.join(d, "snapshots", f.read().strip())
        if os.path.isdir(snap):
            return snap
    except OSError:
        pass
    from huggingface_hub import snapshot_download

    return snapshot_download(repo, local_files_only=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", required=True, choices=["chatterbox", "ming"])
    ap.add_argument("--model", required=True)
    ap.add_argument("--cache-mb", type=int, default=1024)
    ap.add_argument("--interval", type=float, default=0.4)
    a = ap.parse_args()

    t0 = time.perf_counter()
    try:
        import numpy as np
        import mlx.core as mx
        from mlx_audio.tts.utils import load_model

        mx.set_cache_limit(a.cache_mb << 20)
        model = load_model(model_dir(a.model))
    except Exception as e:  # noqa: BLE001
        send({"ev": "error", "msg": f"load failed: {type(e).__name__}: {e}"[:400]})
        return 2

    voice = {"ref": "", "default": getattr(model, "_conds", None)}

    def use_ref(ref):
        ref = os.path.expanduser(ref.strip())
        if ref == voice["ref"]:
            return
        if ref:
            model.prepare_conditionals(ref)
        else:
            model._conds = voice["default"]
        voice["ref"] = ref

    def kwargs(req):
        kw = {}
        if a.engine == "chatterbox":
            kw.update(stream=True, streaming_interval=a.interval)
            use_ref(req.get("ref") or "")
        else:
            kw.update(stream=True, lang_code="en")
            instruct = (req.get("instruct") or "").strip()
            if instruct:
                kw.update(instruct="Speak as: " + instruct, use_zero_spk_emb=True)
            else:
                kw["voice"] = req.get("voice") or "US_Emma"
        return kw

    try:
        for _ in model.generate(text="Ready.", **kwargs({})):
            pass
    except Exception as e:  # noqa: BLE001
        print(f"warm up failed: {e}", file=sys.stderr)
    mx.clear_cache()
    rate = int(getattr(model, "sample_rate", 24000))
    send({"ev": "ready", "rate": rate, "load_ms": int((time.perf_counter() - t0) * 1000)})

    jobs = queue.Queue()
    cancelled = set()

    def reader():
        for line in sys.stdin:
            try:
                req = json.loads(line)
            except ValueError:
                continue
            op = req.get("op")
            if op == "cancel":
                cancelled.add(req.get("id"))
            elif op == "ping":
                send({"ev": "pong"})
            elif op in ("speak", "quit"):
                jobs.put(req)
        jobs.put({"op": "quit"})

    threading.Thread(target=reader, daemon=True).start()

    while True:
        req = jobs.get()
        if req.get("op") == "quit":
            return 0
        rid = req.get("id")
        if rid in cancelled:
            send({"ev": "done", "id": rid, "cancelled": True})
            continue
        stopped = False
        try:
            for r in model.generate(text=req.get("text", ""), **kwargs(req)):
                if rid in cancelled:
                    stopped = True
                    break
                pcm = np.array(r.audio, dtype=np.float32).reshape(-1)
                if pcm.size:
                    send({"ev": "audio", "id": rid, "rate": int(r.sample_rate)}, pcm)
            send({"ev": "done", "id": rid, "cancelled": stopped})
        except Exception as e:  # noqa: BLE001
            send({"ev": "error", "id": rid, "msg": f"{type(e).__name__}: {e}"[:400]})
        cancelled.discard(rid)


if __name__ == "__main__":
    sys.exit(main() or 0)
