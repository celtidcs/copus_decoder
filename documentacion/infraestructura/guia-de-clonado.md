# Guía de clonado

Pasos para tener Copus_Decoder compilado y verificado en una máquina nueva.

## 1. Requisitos

Instala Rust desde <https://rustup.rs> (ver [requisitos.md](requisitos.md)).

## 2. Clonar

```bash
git clone https://github.com/celtidcs/copus_decoder.git
cd copus_decoder
```

## 3. Preparar el entorno

El script comprueba las herramientas, instala `clippy`, `rustfmt` y `cargo-audit` si faltan,
descarga los vectores de RFC 8251 (comprobando su SHA-256), escribe su manifiesto, compila y pasa
las pruebas. Se puede repetir sin efectos secundarios.

Linux / macOS:

```bash
bash documentacion/infraestructura/preparar-entorno.sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -File documentacion\infraestructura\preparar-entorno.ps1
```

## 4. Verificar

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo audit
```

Conformidad con RFC 8251 y red de equivalencia (en modo `--release`; tardan unos minutos):

```bash
cargo test --release --test conformance_rfc -- --ignored rfc_conformance_vectors
cargo test --release --test red_equivalencia -- --ignored
```

Las pruebas buscan los vectores en `material_secundario/vectores/opus_newvectors`, la carpeta que
crean los scripts de preparación. Para usar otra, define `OPUS_TESTVECTORS_DIR` con su ruta.

## 5. Usarlo desde otro proyecto

Ver el [manual de uso](../manual-de-uso.md).
