#!/usr/bin/env bash
# Provisions the Silero engine: a Python environment with torch, and the model.
#
# Silero publishes its TTS models only as PyTorch packages, so the engine runs
# in a sidecar process rather than inside the app. torch is far too large to
# bundle, which is why this lives in a script instead of the installer.
#
# Creates, under the app data directory:
#   python/                 virtualenv with torch (CPU) and scipy
#   models/silero/model.pt  the Silero model
#
# These are the paths the app looks in; see resolve_silero_python and
# resolve_model_dir in src-tauri/src/managers/tts.rs.
#
# Safe to re-run: existing pieces are left alone.
#
# Usage:
#   scripts/setup-silero.sh
#
# The default model, v5_cis_base, is the one Silero publishes under MIT. Their
# other TTS models are CC BY-NC-SA, so overriding SILERO_MODEL_URL may change
# what you are allowed to do with the output:
#
#   SILERO_MODEL_URL=https://models.silero.ai/models/tts/ru/v4_ru.pt \
#     scripts/setup-silero.sh   # smaller and faster, but non-commercial
set -euo pipefail

MODEL_URL="${SILERO_MODEL_URL:-https://models.silero.ai/models/tts/ru/v5_cis_base.pt}"
BASE_PYTHON="${BASE_PYTHON:-python3}"

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*)
    APP_DIR="${APPDATA}/com.rishiskhare.parrot"
    VENV_BIN="Scripts"
    PYTHON_EXE_NAME="python.exe"
    ;;
  Darwin)
    APP_DIR="${HOME}/Library/Application Support/com.rishiskhare.parrot"
    VENV_BIN="bin"
    PYTHON_EXE_NAME="python"
    ;;
  *)
    APP_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/com.rishiskhare.parrot"
    VENV_BIN="bin"
    PYTHON_EXE_NAME="python"
    ;;
esac

VENV_DIR="${APP_DIR}/python"
MODEL_DIR="${APP_DIR}/models/silero"
PYTHON_EXE="${VENV_DIR}/${VENV_BIN}/${PYTHON_EXE_NAME}"

echo "==> App data directory: ${APP_DIR}"
mkdir -p "${MODEL_DIR}"

if [ -x "${PYTHON_EXE}" ] && "${PYTHON_EXE}" -c "import torch, scipy" 2>/dev/null; then
  echo "==> Python environment already present"
else
  echo "==> Creating virtualenv in ${VENV_DIR}"
  "${BASE_PYTHON}" -m venv "${VENV_DIR}"
  echo "==> Installing torch (CPU build) and scipy"
  "${PYTHON_EXE}" -m pip install -q --upgrade pip
  "${PYTHON_EXE}" -m pip install -q torch --index-url https://download.pytorch.org/whl/cpu
  "${PYTHON_EXE}" -m pip install -q scipy
fi

"${PYTHON_EXE}" -c "import torch, scipy; print('    torch', torch.__version__, '| scipy', scipy.__version__)"

if [ -s "${MODEL_DIR}/model.pt" ]; then
  echo "==> Model already present ($(du -h "${MODEL_DIR}/model.pt" | cut -f1))"
else
  echo "==> Downloading ${MODEL_URL}"
  # Download to a temporary name and rename, so an interrupted run cannot leave
  # a truncated model.pt that the app would treat as ready.
  curl -fL --progress-bar -o "${MODEL_DIR}/model.pt.part" "${MODEL_URL}"
  mv "${MODEL_DIR}/model.pt.part" "${MODEL_DIR}/model.pt"
  echo "    done ($(du -h "${MODEL_DIR}/model.pt" | cut -f1))"
fi

echo "==> Verifying the model loads and synthesizes"
"${PYTHON_EXE}" - "${MODEL_DIR}/model.pt" <<'PY'
import sys, warnings
warnings.filterwarnings("ignore")
import torch
from torch.package import PackageImporter

model = PackageImporter(sys.argv[1]).load_pickle("tts_models", "model")
model.to(torch.device("cpu"))
voices = [v for v in getattr(model, "speakers", []) if v.startswith("ru_")] or \
         list(getattr(model, "speakers", []))
audio = model.apply_tts(text="Проверка.", speaker=voices[0],
                        sample_rate=24000, put_accent=True, put_yo=True)
print(f"    ok: {len(voices)} voices, test synthesis {len(audio) / 24000:.2f}s")
PY

echo "==> Ready. Select Silero in Settings -> Models."
