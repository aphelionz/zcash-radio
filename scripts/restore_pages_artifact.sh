#!/usr/bin/env bash
# Restore the latest GitHub Pages artifact into the provided directory.

set -euo pipefail

OUTPUT_DIR="${1:-public}"
SIZE_LIMIT_BYTES=$((200 * 1024 * 1024)) # 200 MB safety cap

if [[ -z "${GH_TOKEN:-}" ]]; then
  echo "GH_TOKEN environment variable is required to restore Pages artifact" >&2
  exit 1
fi

if [[ -z "${GITHUB_REPOSITORY:-}" ]]; then
  echo "GITHUB_REPOSITORY environment variable is required" >&2
  exit 1
fi

APT_UPDATED=0

ensure_tool() {
  local tool="$1"
  if command -v "$tool" >/dev/null 2>&1; then
    return 0
  fi

  if command -v apt-get >/dev/null 2>&1; then
    echo "Installing missing dependency: $tool" >&2
    if [[ $APT_UPDATED -eq 0 ]]; then
      sudo apt-get update >/dev/null
      APT_UPDATED=1
    fi
    sudo apt-get install -y "$tool" >/dev/null
    return 0
  fi

  echo "Required tool '$tool' is missing and automatic installation is unavailable" >&2
  exit 1
}

ensure_tool curl
ensure_tool jq
ensure_tool unzip
ensure_tool rsync

mkdir -p "$OUTPUT_DIR"

owner="${GITHUB_REPOSITORY%/*}"
repo="${GITHUB_REPOSITORY#*/}"
api_base="https://api.github.com/repos/$owner/$repo/pages/artifacts"

echo "Attempting to restore latest Pages artifact into '$OUTPUT_DIR'"

latest_json=$(curl -fsSL \
  -H "Authorization: Bearer $GH_TOKEN" \
  -H "Accept: application/vnd.github+json" \
  "$api_base/latest" || true)

if [[ -z "$latest_json" ]]; then
  echo "No existing Pages artifact found; leaving '$OUTPUT_DIR' as-is" >&2
  exit 0
fi

artifact_id=$(jq -r '.artifact.id // empty' <<<"$latest_json")
if [[ -z "$artifact_id" ]]; then
  echo "No artifact ID found in latest Pages response; leaving '$OUTPUT_DIR' untouched" >&2
  exit 0
fi

artifact_state=$(jq -r '.artifact.state // "unknown"' <<<"$latest_json")
if [[ "$artifact_state" != "active" ]]; then
  echo "Latest Pages artifact ($artifact_id) is not active (state=$artifact_state); aborting restore" >&2
  exit 1
fi

download_json=$(curl -fsSL \
  -H "Authorization: Bearer $GH_TOKEN" \
  -H "Accept: application/vnd.github+json" \
  "$api_base/$artifact_id" || true)
if [[ -z "$download_json" ]]; then
  echo "Unable to fetch download metadata for artifact $artifact_id; leaving '$OUTPUT_DIR' untouched" >&2
  exit 0
fi

download_url=$(jq -r '.download_url // empty' <<<"$download_json")
if [[ -z "$download_url" ]]; then
  echo "Artifact $artifact_id does not expose a download URL; leaving '$OUTPUT_DIR' untouched" >&2
  exit 0
fi

temp_zip=$(mktemp)
trap 'rm -f "$temp_zip"; [[ -n "${temp_dir:-}" ]] && rm -rf "$temp_dir"' EXIT

curl -fsSL -H "Authorization: Bearer $GH_TOKEN" "$download_url" -o "$temp_zip"

zip_size=$(stat -c%s "$temp_zip")
if (( zip_size == 0 )); then
  echo "Downloaded artifact archive is empty; aborting restore" >&2
  exit 1
fi

if (( zip_size > SIZE_LIMIT_BYTES )); then
  echo "Artifact archive exceeds size limit ($zip_size bytes > $SIZE_LIMIT_BYTES bytes); aborting restore" >&2
  exit 1
fi

temp_dir=$(mktemp -d)
unzip -qo "$temp_zip" -d "$temp_dir"

if [[ ! -f "$temp_dir/index.html" ]]; then
  echo "Restored artifact is missing index.html; skipping restore to avoid clobbering site" >&2
  exit 0
fi

if [[ ! -f "$temp_dir/videos.json" ]]; then
  echo "Restored artifact is missing videos.json; skipping restore to keep existing playlist" >&2
  exit 0
fi

if [[ ! -f "$temp_dir/data/zec-stats.json" ]]; then
  echo "Restored artifact is missing data/zec-stats.json; skipping restore to keep existing stats" >&2
  exit 0
fi

rsync -a --delete "$temp_dir"/ "$OUTPUT_DIR"/

echo "Restored Pages artifact $artifact_id into '$OUTPUT_DIR'"
