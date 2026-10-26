# Requisitos

| Herramienta | Versión | Para qué |
|---|---|---|
| Rust (`rustc`, `cargo`) | 1.85 o posterior (`rust-version` de `Cargo.toml`); comprobado con 1.99.1 | Compilar. Instalación: <https://rustup.rs>. |
| `clippy`, `rustfmt` | Los de la cadena instalada | Puertas de estilo. `rustup component add clippy rustfmt`. |
| `cargo-audit` | 0.22 o posterior | Vulnerabilidades de dependencias. `cargo install cargo-audit --locked`. |
| `curl` y `tar` (Linux/macOS) o PowerShell 5.1 y `tar` (Windows 10 o posterior) | Los del sistema | Descargar y extraer los vectores. |
| Espacio en disco | ~200 MB para los vectores (75 MB comprimidos, 121 MB extraídos) más la compilación | |

Sistemas en los que se ejecutan las puertas: Windows 11 y Linux (x86-64). No necesita GPU, red
(salvo para descargar los vectores) ni privilegios de administrador.
