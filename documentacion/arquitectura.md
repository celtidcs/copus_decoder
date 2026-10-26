# Arquitectura

## Módulos

```text
src/
├── lib.rs           API pública: OpusDecoder y OpusError; delega en decoder.rs
├── decoder.rs       Decodificador de un flujo: elige el modo de cada trama (CELT, SILK o
│                    híbrido), encadena las capas, transiciones con redundancia y ocultación
├── multistream.rs   OpusMultistreamDecoder: reparto de subpaquetes y mapa de canales (RFC 7845)
├── packet.rs        Lectura del byte TOC y de las tramas de un paquete (RFC 6716 §3)
├── entropy.rs       Decodificador aritmético de rango (EcDec, RFC 6716 §4.1)
├── pcm.rs           Formato de salida: frecuencias admitidas y saturación a i16
├── error.rs         Error interno, traducido a OpusError en la frontera pública
├── test_vectors.rs  Acceso a los vectores de RFC 8251 desde las pruebas unitarias
├── celt/            Decodificador CELT: energía, asignación de bits, PVQ, bandas, anticolapso,
│                    síntesis MDCT (synthesis.rs), postfiltro y ocultación
└── silk/            Decodificador SILK: índices, NLSF→LPC, excitación, LTP, estéreo,
                     remuestreo y ocultación, con el estado de cada canal en SilkChannel
```

El decodificador sigue la estructura de libopus función a función, en la ruta de coma flotante.
Esa cercanía es deliberada: permite contrastar cada función con la implementación de referencia,
y es la base de la conformidad bit a bit del estado del decodificador de rango (`final_range`).
Las capas dependen hacia abajo: `lib.rs` usa `decoder.rs`, que usa CELT, SILK, `packet.rs`,
`entropy.rs` y `pcm.rs`; `multistream.rs` usa un `OpusDecoder` por flujo y `packet.rs`. Ninguna
capa inferior depende de `decoder.rs`: las colas de redundancia de las tramas solo SILK e híbridas
se leen allí, como en `opus_decode_frame` de libopus.

## Flujo de un paquete

1. `OpusDecoder::decode` / `decode_float` reciben el paquete; uno vacío, o `fec = true`, va a la
   ocultación.
2. `packet.rs` lee el TOC (modo, anchura de banda, duración de trama, estéreo) y separa las tramas.
3. Por cada trama, `decoder.rs` elige CELT, SILK o híbrido, gestiona las transiciones (incluida la
   redundancia CELT en los cambios de modo) y mezcla los canales.
4. CELT y SILK leen sus símbolos del decodificador de rango compartido (`entropy.rs`).
5. La salida se satura a `i16` (`pcm.rs`) o se entrega en `f32`.

## Pruebas

| Archivo | Qué comprueba |
|---|---|
| Pruebas unitarias en `src/` | Funciones internas (tablas, FFT, remuestreo, etc.). |
| `tests/celt_output_rates.rs` | Salida CELT y su ocultación a todas las frecuencias. |
| `tests/multistream.rs` | Mapa de canales, topologías no válidas y equivalencia del multiflujo con un decodificador por flujo. |
| `tests/no_panico.rs` | Entradas aleatorias y mutadas: siempre `Ok` o `Err`, nunca un pánico. |
| `tests/conformance_rfc.rs` | Los 12 vectores de RFC 8251 con `opus_compare` (`tests/compare/`). |
| `tests/red_equivalencia.rs` | 111 huellas de la salida completa (con un caso de transiciones de solo CELT a solo SILK): las de la 0.1.1 original salvo las que cambia a propósito una corrección. |

`tests/common/mod.rs` reúne la lectura de vectores que comparten las pruebas de integración. Las
pruebas que necesitan los vectores de RFC 8251 están marcadas `#[ignore]` porque dependen de
datos externos; la [guía de clonado](infraestructura/guia-de-clonado.md) explica cómo ejecutarlas.

## Decisiones

- **Licencia GPL-3.0 o posterior sobre la rama Apache-2.0 del original.** Es la licencia habitual
  de los proyectos de Celtilander; la Apache-2.0 es compatible con la GPLv3 y la MIT obliga solo a
  conservar el aviso, que se conserva.
- **El original, intacto, como primer commit.** Cualquier cambio se puede comparar con la versión
  publicada en crates.io (`.crate` con SHA-256
  `15ae32c275dabb0cd2c863460bf349e9abc3568ba2abf0f9eb7b7d2edeeed07e`, el mismo que registra el
  índice de crates.io).
- **La red de equivalencia antes de refactorizar.** Las huellas se generaron con la 0.1.1 original
  antes de tocar nada; una refactorización solo se acepta si las deja idénticas. Se comprobó que la
  red detecta un cambio relativo de una millonésima en la recurrencia de desénfasis de CELT
  (`apply_deemph`). Una corrección que cambia la salida a propósito regenera solo las huellas que
  cambia, tras comprobar que la salida se acerca a la de referencia de RFC 8251.
- **Sin `criterion`.** Era una dependencia de desarrollo solo para bancos de velocidad que el
  paquete publicado no incluye.
