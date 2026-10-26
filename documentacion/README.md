# Documentación de Copus_Decoder

Copus_Decoder es una copia corregida y auditada de `opus-decoder` 0.1.1, un decodificador Opus
(RFC 6716 / RFC 8251) escrito en Rust puro, sin `unsafe` ni FFI. La razón de la copia y la
licencia están en el [README principal](../README.md).

| Documento | Para qué sirve |
|---|---|
| [funcionalidades.md](funcionalidades.md) | Qué decodifica, con qué parámetros y qué errores devuelve. |
| [manual-de-uso.md](manual-de-uso.md) | Cómo usarlo como librería, paso a paso. |
| [arquitectura.md](arquitectura.md) | Cómo está organizado el código y por qué. |
| [historial-de-cambios.md](historial-de-cambios.md) | Qué ha cambiado respecto al original. |
| [defectos-conocidos.md](defectos-conocidos.md) | Limitaciones y lo que no está comprobado. |
| [infraestructura/vision-general.md](infraestructura/vision-general.md) | Componentes y entorno de pruebas. |
| [infraestructura/requisitos.md](infraestructura/requisitos.md) | Herramientas y versiones necesarias. |
| [infraestructura/guia-de-clonado.md](infraestructura/guia-de-clonado.md) | Clonar, compilar y verificar desde cero. |

## Garantía de equivalencia

Toda modificación del código se acepta solo si:

1. Las pruebas, clippy (`-D warnings`), `cargo fmt --check` y `cargo audit` pasan en Windows y
   Linux.
2. Los 12 vectores de RFC 8251 pasan la comparación de calidad de `opus_compare`.
3. La red de equivalencia (`tests/red_equivalencia.rs`) da las 111 huellas de
   [`red/huellas.txt`](../red/huellas.txt), generadas con la 0.1.1 original salvo las que cambia a
   propósito cada corrección (listadas en el historial de cambios). Una huella distinta significa
   que la salida cambió en al menos una muestra.
