# Visión general de la infraestructura

Copus_Decoder es una librería Rust sin servicios externos, red ni configuración en tiempo de
ejecución. Lo único externo son los datos de prueba.

| Componente | Dónde | Notas |
|---|---|---|
| Código fuente | `src/`, `tests/` | Crate `copus_decoder`, edición 2024. |
| Dependencia | `thiserror` | Única dependencia, para los errores públicos. |
| Vectores de RFC 8251 | `material_secundario/vectores/` (no versionado) | Se descargan de opus-codec.org con SHA-256 comprobado. |
| Huellas de equivalencia | `red/huellas.txt` | Versionado; generado con la 0.1.1 original. |
| Compilación | `target/` (no versionado) | La carpeta por defecto de cargo. |

No hay variables de entorno obligatorias para compilar, usar la librería ni pasar las pruebas. Las
que necesitan los vectores los buscan en `material_secundario/vectores/opus_newvectors`, o en la
carpeta que indique `OPUS_TESTVECTORS_DIR`.

Las puertas de calidad (pruebas, clippy, formato y `cargo audit`) se ejecutan en Windows y Linux
antes de aceptar cualquier cambio.
