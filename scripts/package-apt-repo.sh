#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./scripts/package-apt-repo.sh [--input-dir <dir>] [--output-dir <dir>] [--suite <suite>] [--component <component>]

Builds an unsigned APT repository layout from cmux_*.deb packages.
EOF
}

INPUT_DIR=""
OUTPUT_DIR=""
SUITE="stable"
COMPONENT="main"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --input-dir)
      INPUT_DIR="${2:-}"
      shift 2
      ;;
    --output-dir)
      OUTPUT_DIR="${2:-}"
      shift 2
      ;;
    --suite)
      SUITE="${2:-}"
      shift 2
      ;;
    --component)
      COMPONENT="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
INPUT_DIR="${INPUT_DIR:-$PROJECT_DIR/dist/deb}"
OUTPUT_DIR="${OUTPUT_DIR:-$PROJECT_DIR/dist/apt}"

command -v dpkg-scanpackages >/dev/null || {
  echo "error: missing required tool: dpkg-scanpackages" >&2
  echo "Install it with: sudo apt-get install dpkg-dev" >&2
  exit 1
}

shopt -s nullglob
packages=("$INPUT_DIR"/cmux_*.deb)
if [[ ${#packages[@]} -eq 0 ]]; then
  echo "error: no cmux_*.deb packages found in $INPUT_DIR" >&2
  exit 1
fi

rm -rf "$OUTPUT_DIR"
install -d -m 0755 \
  "$OUTPUT_DIR/pool/$COMPONENT/c/cmux" \
  "$OUTPUT_DIR/dists/$SUITE/$COMPONENT/binary-amd64"

for package in "${packages[@]}"; do
  install -m 0644 "$package" "$OUTPUT_DIR/pool/$COMPONENT/c/cmux/$(basename "$package")"
done

(
  cd "$OUTPUT_DIR"
  dpkg-scanpackages --arch amd64 "pool/$COMPONENT" /dev/null |
    gzip -9n > "dists/$SUITE/$COMPONENT/binary-amd64/Packages.gz"
)

release_file="$OUTPUT_DIR/dists/$SUITE/Release"
packages_path="$COMPONENT/binary-amd64/Packages.gz"
packages_file="$OUTPUT_DIR/dists/$SUITE/$packages_path"
packages_size="$(stat -c '%s' "$packages_file")"
packages_md5="$(md5sum "$packages_file" | awk '{print $1}')"
packages_sha256="$(sha256sum "$packages_file" | awk '{print $1}')"

cat > "$release_file" <<EOF
Origin: cmux
Label: cmux
Suite: $SUITE
Codename: $SUITE
Date: $(date -Ru)
Architectures: amd64
Components: $COMPONENT
Description: cmux Ubuntu packages
MD5Sum:
 $packages_md5 $packages_size $packages_path
SHA256:
 $packages_sha256 $packages_size $packages_path
EOF

echo "Built unsigned APT repository at $OUTPUT_DIR"
echo
echo "Install locally with:"
echo "  sudo install -d -m 0755 /opt/cmux-apt"
echo "  sudo cp -a $OUTPUT_DIR/. /opt/cmux-apt/"
echo "  echo 'deb [trusted=yes] file:/opt/cmux-apt $SUITE $COMPONENT' | sudo tee /etc/apt/sources.list.d/cmux-local.list"
echo "  sudo apt-get update"
echo "  sudo apt-get install cmux"
