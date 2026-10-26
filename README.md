# Copus_Decoder — Decodificador Opus en Rust puro, corregido y auditado

**Copus_Decoder** es una copia corregida de [`opus-decoder` 0.1.1](https://crates.io/crates/opus-decoder/0.1.1),
el decodificador [Opus](https://opus-codec.org/) en Rust puro de Tadeusz Wójcik
([repositorio original](https://github.com/TadeuszWolfGang/Rusopus)). No es un proyecto nuevo:
es un **parche** sobre esa versión, seguido de una auditoría de código que no cambia la salida
del decodificador.

## Sobre Celtilander

Soy un usuario novel en el mundo de la programación y la IA. He «ayudado a crear» esta app y
espero que a alguien le pueda servir para algo... sin más.

Lo que no funciona no lo escondo: está escrito en
[defectos conocidos](https://github.com/celtidcs/copus_decoder/blob/main/documentacion/defectos-conocidos.md). Si encuentras algo que no esté ahí,
cuéntamelo.

Para realizar esta app he montado un sistema colaborativo y de consenso entre ChatGPT Codex,
Claude Code y Gemini Antigravity. Yo puse la idea y dirigía el proyecto, mientras que ellos
diseñaban, repartían y verificaban el trabajo. Ningún código fue revisado por el agente que lo
había escrito, y una vez terminado todo debía ser aprobado por unanimidad. Hasta que no se
conseguía el consenso, no se daba una tarea por concluida. Cuando por algún motivo no se llegaba
a un consenso, era yo quien tomaba la decisión final, así que los fallos que pueda haber serán
más míos que de ellos.

Saludos a todo el mundo.

---

## 1. Por qué existe esta copia

La versión 0.1.1 publicada en crates.io conserva instrumentación de depuración olvidada: 29
regiones `#region agent log` que abren, en modo de añadir, un archivo de registro: 21 con la ruta
absoluta `/Users/tadeusz/Opus/Rasopus/.cursor/debug-bea564.log` de la máquina del autor, 1 con
`/Users/tadeusz/Opus/Rasopus/.cursor/debug-9a4e1c.log` y 7 con la ruta relativa
`.cursor/debug-bea564.log` (dentro del directorio actual del proceso). Casi todas están detrás de
guardas constantes y nunca se ejecutan, pero **dos se ejecutan en cada trama CELT**, ambas con la
primera ruta absoluta: `postfilter_decoded`, siempre; y
`postfilter_active_but_not_applied_in_rust`, cuando el postfiltro está activo. Si el proceso
puede escribir en esa ruta, el decodificador escribe en disco sin que nadie se lo pida; si no
puede, lo intenta y falla en silencio en cada trama. En Windows, una ruta que empieza por un solo
separador se resuelve en la raíz de la unidad actual
([Microsoft](https://learn.microsoft.com/es-es/dotnet/standard/io/file-path-formats)).

Se avisó al autor en el [issue #8](https://github.com/TadeuszWolfGang/Rusopus/issues/8).

Esta copia:

1. **Quita las 29 regiones**
   ([`parches/0001-quitar-registros-de-depuracion.patch`](parches/0001-quitar-registros-de-depuracion.patch)).
   El decodificador ya no abre ningún archivo.
2. **Demuestra que la salida no cambia**: los 12 vectores oficiales de RFC 8251 siguen pasando, y
   una red de 110 huellas de la salida completa (6 combinaciones de frecuencia y canales, pérdida
   de paquetes, paquetes dañados, salida `i16` y multiflujo) da exactamente lo mismo que la 0.1.1
   original. Desde la 0.1.1 de Copus_Decoder, las correcciones que acercan la salida a libopus
   cambian a propósito solo las huellas afectadas (detalle en el historial de cambios), y la red
   suma una huella de transiciones de solo CELT a solo SILK (111 en total).
3. **Audita el código** (sin valores incrustados, refactorización, piezas de una sola
   responsabilidad y principios SOLID), con esa misma red como garantía de que cada cambio deja
   la salida idéntica.

El historial de git conserva cada paso por separado —el original intacto, el parche y después la
auditoría— para que cualquiera pueda compararlos.

---

## 2. Qué hace

Decodifica paquetes Opus (RFC 6716, con las correcciones de RFC 8251) a PCM:

- Modos CELT, SILK e híbrido.
- Salida a 8, 12, 16, 24 y 48 kHz, mono o estéreo.
- Ocultación de paquetes perdidos (PLC).
- Multiflujo (`OpusMultistreamDecoder`): 5.1, 7.1 o varios flujos mono independientes.
- `#![forbid(unsafe_code)]`, sin FFI ni dependencias de C. Única dependencia: `thiserror`.

No codifica, no lee contenedores (Ogg, WebM), no implementa la corrección de errores en banda
(FEC) y no usa SIMD.

---

## 3. Uso rápido

```toml
[dependencies]
copus_decoder = { git = "https://github.com/celtidcs/copus_decoder" }
```

```rust
use copus_decoder::{OpusDecoder, OpusError};

fn decodificar(paquete: &[u8]) -> Result<usize, OpusError> {
    let mut decodificador = OpusDecoder::new(48_000, 2)?; // 48 kHz, estéreo
    let mut pcm = vec![0i16; decodificador.max_frame_size_per_channel() * 2];
    // Devuelve las muestras decodificadas por canal; un paquete vacío activa la ocultación.
    decodificador.decode(paquete, &mut pcm, false)
}
```

La API es la del original con otro nombre de crate (`opus_decoder::` pasa a ser
`copus_decoder::`). Más detalle en el [manual de uso](documentacion/manual-de-uso.md).

---

## 4. Mapa documental

| Documento | Contenido |
|---|---|
| [`documentacion/README.md`](documentacion/README.md) | Índice y visión general. |
| [`documentacion/funcionalidades.md`](documentacion/funcionalidades.md) | Qué decodifica, con qué límites y qué errores devuelve. |
| [`documentacion/manual-de-uso.md`](documentacion/manual-de-uso.md) | Integración como librería, ocultación de pérdidas y multiflujo. |
| [`documentacion/arquitectura.md`](documentacion/arquitectura.md) | Módulos, flujo de decodificación y decisiones. |
| [`documentacion/historial-de-cambios.md`](documentacion/historial-de-cambios.md) | Cambios respecto al original. |
| [`documentacion/defectos-conocidos.md`](documentacion/defectos-conocidos.md) | Lo que no funciona o no está comprobado. |
| [`documentacion/infraestructura/`](documentacion/infraestructura/) | Requisitos, guía de clonado y scripts de preparación del entorno. |

---

## 5. Verificación

```bash
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo audit
```

La conformidad con RFC 8251 y la red de equivalencia necesitan los vectores oficiales (75 MB
comprimidos; no se incluyen en el repositorio). Los scripts de
[`documentacion/infraestructura/`](documentacion/infraestructura/) los descargan, comprueban su
SHA-256 y preparan el manifiesto; la
[guía de clonado](documentacion/infraestructura/guia-de-clonado.md) explica cómo ejecutar esas
pruebas.

---

## 6. Licencia y atribución

Copus_Decoder se distribuye bajo la **GNU General Public License v3.0 o posterior**
([`LICENSE`](LICENSE)).

- El código original es © 2026 Tadeusz Wójcik, con licencia doble MIT o Apache-2.0. Esta copia lo
  recibe bajo la rama **Apache-2.0**, que la Apache Software Foundation declara compatible con la
  GPLv3 ([fuente](https://www.apache.org/licenses/GPL-compatibility.html)). Sus textos de licencia
  se conservan en [`licencias/`](licencias/), y [`AVISO.md`](AVISO.md) indica qué se ha
  modificado, como exige el apartado 4(b) de la licencia Apache.
- El original es un port de [libopus](https://gitlab.xiph.org/xiph/opus) (© Xiph.Org y otros,
  licencia BSD de 3 cláusulas), cuyo aviso se conserva en
  [`licencias/LIBOPUS-COPYING-BSD-3`](licencias/LIBOPUS-COPYING-BSD-3). Opus está sujeto a las
  licencias de patentes gratuitas que se citan en ese archivo.
