#!/usr/bin/env bash
# Downloads the native libraries Kokoro needs at runtime into src-tauri/resources.
#
# These are binaries, so they are fetched rather than committed. CI does the same
# thing before building; run this once before a local build.
#
#   onnxruntime/  the ONNX Runtime shared library. ort is built in load-dynamic
#                 mode, so it opens this at runtime instead of linking it in —
#                 the prebuilt static library needs a newer MSVC toolset than
#                 some build environments have.
#   espeak-ng/    the phonemizer Kokoro feeds its text through.
#
# espeak-ng-data/ is committed, being platform-independent data rather than code.
set -euo pipefail

ORT_VERSION="${ORT_VERSION:-1.22.0}"
ESPEAK_VERSION="${ESPEAK_VERSION:-1.51}"

cd "$(dirname "$0")/.."
RESOURCES="src-tauri/resources"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) PLATFORM=windows ;;
  Darwin) PLATFORM=macos ;;
  *) PLATFORM=linux ;;
esac

echo "==> ONNX Runtime ${ORT_VERSION} (${PLATFORM})"
mkdir -p "${RESOURCES}/onnxruntime"
case "${PLATFORM}" in
  windows)
    ORT_ARCHIVE="onnxruntime-win-x64-${ORT_VERSION}.zip"
    curl -fL --progress-bar -o "${WORK}/ort.zip" \
      "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ORT_ARCHIVE}"
    unzip -q -o "${WORK}/ort.zip" -d "${WORK}/ort"
    find "${WORK}/ort" -name "onnxruntime*.dll" -exec cp {} "${RESOURCES}/onnxruntime/" \;
    ;;
  macos)
    ORT_ARCHIVE="onnxruntime-osx-universal2-${ORT_VERSION}.tgz"
    curl -fL --progress-bar -o "${WORK}/ort.tgz" \
      "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ORT_ARCHIVE}"
    tar -xzf "${WORK}/ort.tgz" -C "${WORK}"
    find "${WORK}" -name "libonnxruntime*.dylib" -exec cp {} "${RESOURCES}/onnxruntime/" \;
    ;;
  linux)
    ORT_ARCHIVE="onnxruntime-linux-x64-${ORT_VERSION}.tgz"
    curl -fL --progress-bar -o "${WORK}/ort.tgz" \
      "https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/${ORT_ARCHIVE}"
    tar -xzf "${WORK}/ort.tgz" -C "${WORK}"
    find "${WORK}" -name "libonnxruntime.so*" -exec cp {} "${RESOURCES}/onnxruntime/" \;
    ;;
esac
ls -1 "${RESOURCES}/onnxruntime/"

if [ "${PLATFORM}" = "windows" ]; then
  echo "==> espeak-ng ${ESPEAK_VERSION}"
  mkdir -p "${RESOURCES}/espeak-ng"
  curl -fL --progress-bar -o "${WORK}/espeak.msi" \
    "https://github.com/espeak-ng/espeak-ng/releases/download/${ESPEAK_VERSION}/espeak-ng-X64.msi"
  # Administrative extraction: unpacks the payload without installing or
  # needing elevation.
  msiexec //a "$(cygpath -w "${WORK}/espeak.msi")" //qn \
    TARGETDIR="$(cygpath -w "${WORK}/espeak")"
  # msiexec detaches, so wait for the payload to appear.
  for _ in $(seq 1 30); do
    [ -f "${WORK}/espeak/eSpeak NG/espeak-ng.exe" ] && break
    sleep 1
  done
  cp "${WORK}/espeak/eSpeak NG/espeak-ng.exe" "${RESOURCES}/espeak-ng/"
  cp "${WORK}/espeak/eSpeak NG/"*.dll "${RESOURCES}/espeak-ng/"
  ls -1 "${RESOURCES}/espeak-ng/"
else
  echo "==> espeak-ng: install it with your package manager (brew/apt install espeak-ng)"
fi

echo "==> Done."
