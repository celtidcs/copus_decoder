#!/usr/bin/env bash
# Prepara un entorno Linux/macOS para compilar y verificar Copus_Decoder.
# Idempotente: se puede ejecutar varias veces; lo que ya está hecho no se repite.
#
# Uso (desde cualquier carpeta):  bash documentacion/infraestructura/preparar-entorno.sh
set -euo pipefail

# Raíz del proyecto: dos niveles por encima de este script.
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

# Versión mínima de Rust que declara Cargo.toml (rust-version).
RUST_MINIMA="1.85"
# Vectores oficiales de conformidad de RFC 8251 y su SHA-256 conocido.
URL_VECTORES="https://opus-codec.org/static/testvectors/opus_testvectors-rfc8251.tar.gz"
SHA_VECTORES="6b26a22f9ba87b2b836906a9bb7afec5f8e54d49553b1200382520ee6fedfa55"
# Carpeta local, fuera del control de versiones (material_secundario/ está en .gitignore).
CARPETA_VECTORES="material_secundario/vectores"
TAR_VECTORES="$CARPETA_VECTORES/opus_testvectors-rfc8251.tar.gz"
DIR_VECTORES="$CARPETA_VECTORES/opus_newvectors"
NUMERO_DE_VECTORES=12

paso() { printf '\n== %s\n' "$1"; }
falla() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

paso "1. Sistema y herramientas"
uname -a
for herramienta in cargo rustc curl tar; do
  command -v "$herramienta" >/dev/null || falla "falta '$herramienta' (Rust: https://rustup.rs)"
done
version_rust="$(rustc --version | awk '{print $2}')"
printf 'rustc %s (mínima %s)\n' "$version_rust" "$RUST_MINIMA"
[ "$(printf '%s\n%s\n' "$RUST_MINIMA" "$version_rust" | sort -V | head -1)" = "$RUST_MINIMA" ] \
  || falla "rustc $version_rust es anterior a $RUST_MINIMA: ejecuta 'rustup update'"
if command -v sha256sum >/dev/null; then SHA="sha256sum"; else SHA="shasum -a 256"; fi

paso "2. Componentes de Rust y cargo-audit"
rustup component add clippy rustfmt
cargo audit --version >/dev/null 2>&1 || cargo install cargo-audit --locked

paso "3. Carpeta de los vectores"
mkdir -p "$CARPETA_VECTORES"

paso "4. Vectores de RFC 8251"
if [ ! -f "$TAR_VECTORES" ]; then
  curl -fL --proto '=https' -o "$TAR_VECTORES.parcial" "$URL_VECTORES"
  mv "$TAR_VECTORES.parcial" "$TAR_VECTORES"
fi
sha_real="$($SHA "$TAR_VECTORES" | awk '{print $1}')"
[ "$sha_real" = "$SHA_VECTORES" ] \
  || falla "SHA-256 inesperado de $TAR_VECTORES ($sha_real); bórralo y repite"
[ -d "$DIR_VECTORES" ] || tar -xzf "$TAR_VECTORES" -C "$CARPETA_VECTORES"
# Manifiesto que leen las pruebas: los 12 vectores, comparados en estéreo a 48 kHz.
: > "$DIR_VECTORES/vectors.txt"
for n in $(seq -w 1 "$NUMERO_DE_VECTORES"); do
  echo "testvector$n testvector$n.bit testvector$n.dec 48000 2" >> "$DIR_VECTORES/vectors.txt"
done
echo "vectores listos en $DIR_VECTORES"

paso "5. Compilación"
cargo build --release --all-targets

paso "6. Comprobación final"
cargo test --release --all-features

paso "Listo"
cat <<FIN
Pruebas con los vectores (conformidad RFC 8251, diferencia frente a libopus y red de equivalencia):
  export OPUS_TESTVECTORS_DIR="$RAIZ/$DIR_VECTORES"
  cargo test --release --test conformance_rfc -- --ignored rfc_conformance_vectors
  cargo test --release --test diferencia_libopus -- --ignored
  cargo test --release --test red_equivalencia -- --ignored
FIN
