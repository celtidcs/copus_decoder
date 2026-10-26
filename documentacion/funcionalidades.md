# Funcionalidades

## Decodificador de un flujo: `OpusDecoder`

| Operación | Comportamiento |
|---|---|
| `OpusDecoder::new(frecuencia, canales)` | Frecuencia de salida 8000, 12000, 16000, 24000 o 48000 Hz; 1 o 2 canales. Cualquier otro valor devuelve `OpusError::InvalidArgument`. |
| `decode(paquete, pcm_i16, fec)` | Decodifica a PCM de 16 bits, entrelazado si es estéreo. Devuelve las muestras **por canal**. |
| `decode_float(paquete, pcm_f32, fec)` | Igual, en coma flotante (ruta nativa del decodificador). |
| `reset()` | Reinicia el estado interno del decodificador (por ejemplo, tras un salto en el flujo). |
| `max_frame_size_per_channel()` | Tamaño máximo de trama por canal a la frecuencia elegida: dimensiona el búfer de salida. |
| `final_range()` | Estado final del decodificador aritmético tras el último paquete (lo usa la conformidad RFC). |

Un `paquete` vacío, o `fec = true`, activa la **ocultación de paquetes perdidos** (PLC): el
decodificador extrapola a partir del estado anterior (CELT con atenuación progresiva; SILK con
extrapolación de tono). La corrección de errores en banda (FEC/LBRR) no está implementada:
`fec = true` se trata igual que una pérdida.

Modos admitidos: CELT, SILK, híbrido, y las transiciones entre ellos, en todas las anchuras de
banda que define Opus. El estéreo se mezcla a mono, o el mono a estéreo, según los canales pedidos.

## Decodificador multiflujo: `OpusMultistreamDecoder`

`new(frecuencia, canales, flujos, flujos_acoplados, mapa)`: los primeros `flujos_acoplados` flujos
son estéreo y el resto mono. `mapa[i]` indica qué canal decodificado va a la salida `i`; el valor
255 la deja en silencio. Sirve para 5.1/7.1 o para juntar varios flujos mono independientes (por
ejemplo, las dos partes de una llamada). Tiene `decode`, `decode_float` y `reset` con la misma
semántica que `OpusDecoder`.

## Errores: `OpusError`

| Variante | Cuándo |
|---|---|
| `InvalidPacket` | Paquete mal formado, incoherente o demasiado grande. |
| `BufferTooSmall` | El búfer de salida no cabe la trama decodificada. |
| `InvalidArgument(&str)` | Parámetro de construcción no válido; el texto nombra el parámetro. |
| `InternalError` | Estado interno no admitido. |

## Lo que no hace

No codifica, no lee contenedores (Ogg, WebM, Matroska), no implementa FEC en banda y no usa SIMD.
