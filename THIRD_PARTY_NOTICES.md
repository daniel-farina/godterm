# Third party notices

Models and libraries GodTerm downloads or links that carry their own
terms.

## WeSpeaker ResNet34-LM speaker model

- Used for: the speaker lock (voice embeddings), fetched by
  `godterm voice install-speaker` to
  `~/.cache/godterm-models/wespeaker_en_voxceleb_resnet34_LM.onnx`.
- Source: https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx
  (the ONNX export by sherpa-onnx of WeSpeaker's VoxCeleb ResNet34-LM,
  https://github.com/wenet-e2e/wespeaker).
- sha256: `e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012`
  (26,530,550 bytes).
- License: Creative Commons Attribution 4.0 International (CC BY 4.0).
  WeSpeaker's pretrained models follow the license of their training data;
  VoxCeleb is CC BY 4.0 (https://mm.kaist.ac.kr/datasets/voxceleb/). The
  WeSpeaker code is Apache 2.0.
- Attribution: WeSpeaker (Wang et al., "Wespeaker: A research and
  production oriented speaker embedding learning toolkit", ICASSP 2023);
  VoxCeleb2 (Chung, Nagrani, Zisserman, 2018).

Other ONNX speaker embedding models taking 80 bin Kaldi fbank features can
be used through `voice.speaker_model = "/path/to/model.onnx"` (for
example 3D-Speaker CAM++ or ERes2Net exports from the same sherpa-onnx
release, Apache 2.0); retrain your voice after changing it.

## nnnoiseless (RNNoise)

- Used for: noise suppression before the endpointer (`voice.denoise`).
- Source: https://crates.io/crates/nnnoiseless, a Rust port of RNNoise
  (https://github.com/xiph/rnnoise) with its trained weights.
- License: BSD 3-Clause.

## Kokoro v1.0 text to speech

- Used for: talking back (`voice.tts_engine = "kokoro"`). GodTerm runs the
  model itself through ONNX Runtime; the model and voices are downloaded by
  the user (`kokoro-v1.0.onnx` and `voices-v1.0.bin` from
  https://github.com/thewh1teagle/kokoro-onnx/releases, model-files-v1.0).
- Model: Kokoro-82M by hexgrad, https://huggingface.co/hexgrad/Kokoro-82M,
  Apache 2.0. The ONNX export (kokoro-onnx by thewh1teagle) is MIT.
- Phonemes come from espeak-ng, run as a separate program the user installs
  (`brew install espeak-ng`); espeak-ng is GPL 3.0 and is not linked into
  or shipped with GodTerm.

## whisper.cpp and Whisper models

- Used for: speech to text (`whisper-cli` / `whisper-server`, installed by
  the user with `brew install whisper-cpp`, run as separate programs).
- whisper.cpp by Georgi Gerganov, https://github.com/ggml-org/whisper.cpp,
  MIT. Models (for example `ggml-large-v3-turbo.bin` from
  https://huggingface.co/ggerganov/whisper.cpp) are OpenAI's Whisper
  weights, MIT.

## Silero VAD

- Used for: voice activity detection in whisper-server
  (`ggml-silero-v5.1.2.bin` from https://huggingface.co/ggml-org/whisper-vad).
- Silero VAD by Silero Team, https://github.com/snakers4/silero-vad, MIT.

## ONNX Runtime

- Used for: running Kokoro and the speaker model, statically linked through
  the `ort` crate (prebuilt binaries from pyke).
- ONNX Runtime by Microsoft, https://github.com/microsoft/onnxruntime, MIT.

## Apple Speech framework

- Used for: on device recognition on macOS 26 (`godterm-speech`), a system
  framework; no model is shipped.

## Rust crates

GodTerm statically links about 390 crates from crates.io. Their licenses
are permissive: MIT and/or Apache 2.0 for almost all, plus BSD 2 and 3
Clause, ISC, Zlib, Unicode 3.0, 0BSD, Unlicense and BlueOak (each as an
option alongside MIT or Apache 2.0), CDLA Permissive 2.0 (the Mozilla CA
certificate data in `webpki-roots`), and MPL 2.0 (`option-ext`, used
unmodified). Run `cargo metadata` (or `cargo about`) for the full list with
each crate's license text.
